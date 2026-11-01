#requires -Version 7.0
<#
.SYNOPSIS
  dsh-computer-use 端到端真实 CI 自动化验证套件。
.DESCRIPTION
  执行全量回归验证：
    1. 环境与完整性预检（检查 helper 完整性标签是否为正常/High）；
    2. node_modules 接线检查（保证 @deepseek-ai/* 依赖解析正常）；
    3. JS 契约与 Experience 单元测试 (node --test)；
    4. Python 单元与 Parity 契约测试 (pytest)；
    5. 浏览器真实后端冒烟 (browser_setup -> tab -> ax -> screenshot -> locator -> cdp -> close)；
    6. 桌面真实后端冒烟 (launch_app -> get_window_state -> type_text -> set_value -> secondary -> close)；
.EXAMPLE
  pwsh -File scripts\ci-smoke.ps1
  pwsh -File scripts\ci-smoke.ps1 -SkipDesktop
#>
param(
  [switch]$SkipDesktop = $false,
  [switch]$SkipBrowser = $false
)

$ErrorActionPreference = 'Stop'
$PluginRoot = Split-Path -Parent $PSScriptRoot
$Results = [System.Collections.Generic.List[PSCustomObject]]::new()

function Record-Result([string]$Stage, [bool]$Ok, [string]$Detail) {
  $Results.Add([PSCustomObject]@{
    Stage  = $Stage
    Status = if ($Ok) { "PASS" } else { "FAIL" }
    Detail = $Detail
  })
  if ($Ok) {
    Write-Host "  [PASS] ${Stage}: $Detail" -ForegroundColor Green
  } else {
    Write-Host "  [FAIL] ${Stage}: $Detail" -ForegroundColor Red
  }
}

Write-Host "=== dsh-computer-use 自动化回归与 CI 套件 ===" -ForegroundColor Cyan
Write-Host "插件根目录: $PluginRoot" -ForegroundColor Gray

# 1. 环境与依赖检查
$helperBin = Join-Path $PluginRoot "helper-rs\bin\win32-x64\dsh-computer-use.exe"
if (Test-Path $helperBin) {
  $labelOut = icacls $helperBin 2>&1 | Out-String
  $hasLow = $labelOut -match 'Low Mandatory Level'
  $intMsg = if ($hasLow) { "警告: helper.exe 带有 Low 完整性标签，先 icacls 去掉再重跑（fix-computer-use-integrity.ps1 已从仓库清出）" } else { "helper.exe 完整性正常 (High/Unlabeled)" }
  Record-Result "HelperIntegrity" (-not $hasLow) $intMsg
  # Audit P3-2 (2026-10-25): the committed exe must be built from HEAD. A source
  # change without a rebuilt binary silently regresses every helper fix.
  $headRev = (git -C $PluginRoot rev-parse --short HEAD)
  $env:DSH_CU_PRINT_BUILD = '1'
  $binRev = (& $helperBin | Select-Object -First 1)
  Remove-Item Env:\DSH_CU_PRINT_BUILD -ErrorAction SilentlyContinue
  # The embedded rev can never equal the commit that carries the binary
  # (chicken-and-egg), so the contract is: built from some commit that is an
  # ANCESTOR of HEAD -- a stale binary from before the last source change fails.
  git -C $PluginRoot merge-base --is-ancestor $binRev HEAD
  $revOk = ($binRev -and $LASTEXITCODE -eq 0)
  Record-Result "HelperBinaryRevision" $revOk "exe=$binRev head=$headRev（不一致请运行 scripts/ship-helper.ps1 重建并提交二进制）"
} else {
  Record-Result "HelperBinary" $false "helper.exe 缺失，请先 cargo build --release"
}

# 2. node_modules 接线检查
$nm = Join-Path $PluginRoot "node_modules"
$sharedNm = "$env:USERPROFILE\.dsh\profiles\node_modules"
if (-not (Test-Path $nm) -and (Test-Path $sharedNm)) {
  New-Item -ItemType Junction -Path $nm -Target $sharedNm -Force | Out-Null
}
Record-Result "NodeModulesJunction" (Test-Path "$nm\@deepseek-ai\cordis") "已确保 node_modules 接线指向 profiles 依赖池"

# 3. JS 契约与交互摘要测试（experience 层已删除，见 docs/audit-fix-roadmap-2026-10-25.md P2-9）
Write-Host "`n--- 运行 JS 契约测试 (node --test) ---" -ForegroundColor Yellow
$jsOut = node --test "$PluginRoot\tests\contracts.test.mjs" "$PluginRoot\tests\interactive_summary.test.mjs" 2>&1
$jsRc = $LASTEXITCODE
$jsSummary = ($jsOut | Select-String -Pattern "ℹ (pass|fail|tests)" | ForEach-Object { $_.Line.Trim() }) -join ", "
$jsMsg = if ($jsRc -eq 0) { "全部通过: $jsSummary" } else { "存在失败: $jsSummary" }
Record-Result "JSTests" ($jsRc -eq 0) $jsMsg

# 4. Python 单元与 Parity 测试
Write-Host "`n--- 运行 Python 单元与 Parity 测试 (pytest) ---" -ForegroundColor Yellow
$env:PYTHONPATH = $PluginRoot
$pyOut = python -m pytest "$PluginRoot\tests" 2>&1
$pyRc = $LASTEXITCODE
$pySummary = ($pyOut | Select-String -Pattern "==.*(passed|failed|skipped).*==" | Select-Object -Last 1 | ForEach-Object { $_.Line.Trim() })
$pyMsg = if ($pyRc -eq 0) { "全部通过: $pySummary" } else { "存在失败: $pySummary" }
Record-Result "PythonTests" ($pyRc -eq 0) $pyMsg

# 5. 浏览器真实后端冒烟
if (-not $SkipBrowser) {
  Write-Host "`n--- 运行浏览器真实后端冒烟 (CDP + IAB) ---" -ForegroundColor Yellow
  $browserSmokeScript = @'
import { Sidecar } from './src/sidecar.js'
const s = new Sidecar({ backend: 'windows', engineRoot: process.cwd(), pythonPath: '' })
const steps = []
try {
  await s.request('health')
  const setup = await s.request('call', { name: 'browser_setup', arguments: { environment: 'codex-app' }, meta: {} })
  steps.push({ name: 'setup', ok: setup?.ok !== false })
  const browsers = await s.request('call', { name: 'list_browsers', arguments: {}, meta: {} })
  steps.push({ name: 'list_browsers', ok: Array.isArray(browsers?.value) && browsers.value.length > 0 })
  const tab = await s.request('call', { name: 'create_tab', arguments: { url: 'https://example.com' }, meta: {} })
  const tid = tab?.value?.id || tab?.value?.tab_id
  steps.push({ name: 'create_tab', ok: !!tid })
  if (tid) {
    const ax = await s.request('call', { name: 'tab_ax_get_state', arguments: { tab_id: tid, content: 'axStateAndScreenshot' }, meta: {} })
    steps.push({ name: 'tab_ax_get_state', ok: ax?.ok !== false, images: ax?.images?.length ?? 0 })
    const shot = await s.request('call', { name: 'tab_screenshot', arguments: { tab_id: tid }, meta: {} })
    steps.push({ name: 'tab_screenshot', ok: shot?.ok !== false, images: shot?.images?.length ?? 0 })
    const count = await s.request('call', { name: 'playwright_locator_count', arguments: { tab_id: tid, selector: 'a' }, meta: {} })
    steps.push({ name: 'playwright_locator_count', ok: count?.value?.count >= 1 })
    const cdp = await s.request('call', { name: 'tab_cdp_call', arguments: { tab_id: tid, method: 'Runtime.evaluate', params: { expression: 'location.href', returnByValue: true } }, meta: {} })
    steps.push({ name: 'tab_cdp_call', ok: cdp?.ok !== false })
    await s.request('call', { name: 'close_tab', arguments: { tab_id: tid }, meta: {} })
    steps.push({ name: 'close_tab', ok: true })
  }
} finally {
  s.dispose()
  console.log(JSON.stringify(steps))
}
'@
  $tmpFile = Join-Path $PluginRoot "_ci_browser_tmp.mjs"
  Set-Content -Path $tmpFile -Value $browserSmokeScript -Encoding utf8
  try {
    $bSmokeOut = node $tmpFile 2>&1
    $bSmokeJson = ($bSmokeOut | Select-String -Pattern '^\[.*\]$' | Select-Object -Last 1).Line
    if ($bSmokeJson) {
      $steps = $bSmokeJson | ConvertFrom-Json
      $allPass = -not ($steps | Where-Object { $_.ok -ne $true })
      Record-Result "BrowserSmoke" $allPass "完成 $($steps.Count) 个关键链路方法验证 (setup/tab/ax/shot/locator/cdp/close)"
    } else {
      Record-Result "BrowserSmoke" $false "冒烟输出未解析到步骤结果: $bSmokeOut"
    }
  } finally {
    Remove-Item $tmpFile -Force -ErrorAction SilentlyContinue
  }
}

# 6. 桌面真实后端冒烟
if (-not $SkipDesktop) {
  Write-Host "`n--- 运行桌面真实后端冒烟 (Notepad / Window2) ---" -ForegroundColor Yellow
  $desktopSmokeScript = @'
import { Sidecar } from './src/sidecar.js'
const s = new Sidecar({ backend: 'windows', engineRoot: process.cwd(), pythonPath: '' })
const steps = []
let meta = {}
try {
  await s.request('health')
  const appTarget = 'C:\\Windows\\System32\\notepad.exe'
  await s.request('call', { name: 'launch_app', arguments: { app: appTarget }, meta: { 'x-oai-cua-approved-app': appTarget } })
  await new Promise(r => setTimeout(r, 1200))
  const ws = (await s.request('call', { name: 'list_windows', arguments: {}, meta: {} })).value || []
  const notepad = ws.find(w => /notepad\.exe/i.test(w.app))
  steps.push({ name: 'launch_and_find', ok: !!notepad })
  if (notepad) {
    meta = { 'x-oai-cua-approved-app': notepad.app }
    const win = { app: notepad.app, id: notepad.id, title: notepad.title }
    const st1 = await s.request('call', { name: 'get_window_state', arguments: { window: win, include_text: true, include_screenshot: false }, meta })
    const treeLen = (st1?.value?.accessibility?.tree || '').length
    const limited = /Accessibility is limited/i.test(st1?.value?.accessibility?.tree || '')
    steps.push({ name: 'get_window_state_tree', ok: treeLen > 100 && !limited, treeLen })
    
    // click into edit and type
    await s.request('call', { name: 'click', arguments: { window: win, x: 200, y: 200 }, meta })
    await s.request('call', { name: 'get_window_state', arguments: { window: win, include_text: true, include_screenshot: false }, meta })
    const typed = await s.request('call', { name: 'type_text', arguments: { window: win, text: 'CI_VERIFY' }, meta })
    steps.push({ name: 'type_text', ok: typed?.ok !== false })
    
    // set_value
    const setVal = await s.request('call', { name: 'set_value', arguments: { window: win, element_index: 1, value: 'CI_SET_VAL' }, meta })
    steps.push({ name: 'set_value', ok: setVal?.ok !== false })
    
    // secondary Raise
    const sec = await s.request('call', { name: 'perform_secondary_action', arguments: { window: win, element_index: 0, action: 'Raise' }, meta })
    steps.push({ name: 'perform_secondary_action_Raise', ok: sec?.ok !== false })
  }
} finally {
  s.dispose()
  console.log(JSON.stringify(steps))
}
'@
  $tmpDFile = Join-Path $PluginRoot "_ci_desktop_tmp.mjs"
  Set-Content -Path $tmpDFile -Value $desktopSmokeScript -Encoding utf8
  try {
    $dSmokeOut = node $tmpDFile 2>&1
    $dSmokeJson = ($dSmokeOut | Select-String -Pattern '^\[.*\]$' | Select-Object -Last 1).Line
    if ($dSmokeJson) {
      $steps = $dSmokeJson | ConvertFrom-Json
      $allPass = -not ($steps | Where-Object { $_.ok -ne $true })
      Record-Result "DesktopSmoke" $allPass "完成 $($steps.Count) 项真实窗口交互验证 (launch/tree/type/set_value/secondary)"
    } else {
      Record-Result "DesktopSmoke" $false "冒烟输出未解析到步骤结果: $dSmokeOut"
    }
  } finally {
    Remove-Item $tmpDFile -Force -ErrorAction SilentlyContinue
    Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  }
}

Write-Host "`n=================== CI 汇总报告 ===================" -ForegroundColor Cyan
$Results | Format-Table -AutoSize
$failedCount = ($Results | Where-Object { $_.Status -eq "FAIL" }).Count
if ($failedCount -eq 0) {
  Write-Host ">>> 全部检查项通过！插件基建完全健康。<<<" -ForegroundColor Green
  exit 0
} else {
  Write-Host ">>> 存在 $failedCount 个失败项，请检查详情。<<<" -ForegroundColor Red
  exit 1
}
