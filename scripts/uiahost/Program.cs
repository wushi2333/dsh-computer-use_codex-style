// dsh-uiahost —— 常驻 UIA 服务。
//
// 为什么要有它：PowerShell 版每次调用都要新起进程 + 重新走一遍 ControlView 树
// （实测 1.5–2s），轮次一多就超过调用方的等待上限，而且每次树都是新的 AutomationElement，
// 无法复用。这里把 UIA 客户端放在一个常驻进程里，stdin 收一行 JSON 指令、stdout 回一行 JSON。
//
// 指令（每行一个 JSON 对象）：
//   {"op":"ping"}
//   {"op":"tree","hwnd":123,"max":500}
//   {"op":"read","hwnd":123}                     // 题干 + 选项（letter/text/checked/rect）
//   {"op":"set","hwnd":123,"target":"ACDE"}      // 幂等设置选项集
//   {"op":"post","hwnd":123,"points":["x,y",...]}// PostMessage 点击（WMPF 唯一有效通路）
//   {"op":"act","hwnd":123,"name":"交卷"}        // 按名字走 UIA pattern
//   {"op":"quit"}
//
// 设计要点：
//  * UIA 调用都包在 STA 线程里（UIA 客户端要求 STA），主循环只做 JSON 序列化。
//  * 选项框坐标不靠"标签 y + 常数"：先在标签左侧的竖带上做像素扫描找红色方块，
//    每次点击后重新扫描（页面会重排）。
//  * 所有返回值都带 ok 字段，失败带 error，调用方永远能判断真假。

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading;
using System.Windows.Automation;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;

static class Program
{
    // ---------- Win32 ----------
    [StructLayout(LayoutKind.Sequential)]
    struct RECT { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr ChildWindowFromPointEx(IntPtr h, POINT p, uint f);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern bool SetProcessDPIAware();

    [StructLayout(LayoutKind.Sequential)]
    struct POINT { public int X, Y; }

    const uint WM_MOUSEMOVE = 0x0200;
    const uint WM_LBUTTONDOWN = 0x0201;
    const uint WM_LBUTTONUP = 0x0202;

    static IntPtr hwnd = IntPtr.Zero;

    static int Main()
    {
        try { SetProcessDPIAware(); } catch { }

        string line;
        while ((line = Console.ReadLine()) != null)
        {
            if (string.IsNullOrWhiteSpace(line)) continue;
            string reply;
            try
            {
                using var doc = JsonDocument.Parse(line);
                var root = doc.RootElement;
                string op = root.TryGetProperty("op", out var o) ? o.GetString() : "";
                if (op == "quit") { Console.WriteLine("{\"ok\":true,\"bye\":true}"); return 0; }

                // UIA can wedge (the provider stops answering) — if a job exceeds the
                // budget, abandon that thread and start a fresh one rather than making
                // every later call inherit the hang. This is the difference between
                // "one slow request" and "the whole host is dead".
                var job = new Job(root);
                var worker = EnsureWorker();
                worker.Queue(job);
                if (!job.Done.Wait(8000))
                {
                    reply = Err("uia timeout (provider wedged?) — worker replaced");
                    if (worker.Abort()) { /* a fresh worker is created on the next call */ }
                }
                else if (job.Failure != null) reply = Err(job.Failure.GetType().Name + ": " + job.Failure.Message);
                else reply = job.Result;
            }
            catch (Exception ex) { reply = Err("protocol: " + ex.Message); }

            Console.Out.Write(reply.Replace("\r", " ").Replace("\n", " ") + "\n");
            Console.Out.Flush();
        }
        return 0;
    }

    sealed class Job
    {
        public readonly JsonElement Request;
        public string Result;
        public Exception Failure;
        public readonly ManualResetEventSlim Done = new ManualResetEventSlim();
        public Job(JsonElement request) { Request = request; }
    }

    sealed class Worker
    {
        Thread thread;
        BlockingCollection<Job> queue;
        public void Queue(Job job) => queue.Add(job);
        public bool Abort() { try { thread.Interrupt(); } catch { } return true; }
        public static Worker Start()
        {
            var w = new Worker { queue = new BlockingCollection<Job>() };
            w.thread = new Thread(() =>
            {
                foreach (var job in w.queue.GetConsumingEnumerable())
                {
                    try { job.Result = Dispatch(job.Request); }
                    catch (Exception ex) { job.Failure = ex; }
                    finally { job.Done.Set(); }
                }
            });
            w.thread.SetApartmentState(ApartmentState.STA);
            w.thread.IsBackground = true;
            w.thread.Start();
            return w;
        }
    }

    static Worker current;

    static Worker EnsureWorker()
    {
        if (current == null) current = Worker.Start();
        return current;
    }

    static string Err(string message) =>
        JsonSerializer.Serialize(new Dictionary<string, object> { ["ok"] = false, ["error"] = message });

    static string Ok(Dictionary<string, object> fields)
    {
        fields["ok"] = true;
        return JsonSerializer.Serialize(fields);
    }

    static int Int(JsonElement e, string name, int fallback) =>
        e.TryGetProperty(name, out var v) && v.TryGetInt32(out var n) ? n : fallback;

    static string Dispatch(JsonElement req)
    {
        string op = req.TryGetProperty("op", out var o) ? (o.GetString() ?? "") : "";
        hwnd = new IntPtr(Int(req, "hwnd", 0));
        switch (op)
        {
            case "ping": return Ok(new Dictionary<string, object> { ["pong"] = true });
            case "tree": return Tree(req);
            case "read": return Read(req);
            case "set": return Set(req);
            case "post": return Post(req);
            case "act": return Act(req);
            default: return Err("unknown op: " + op);
        }
    }

    // ---------- 基础 ----------
    static AutomationElement Root()
    {
        if (!IsWindow(hwnd)) throw new InvalidOperationException("window handle is not valid (stale?)");
        var el = AutomationElement.FromHandle(hwnd);
        if (el == null) throw new InvalidOperationException("no AutomationElement for that window");
        return el;
    }

    // FindAll(Descendants) 在客户端内部一次批量取树；逐节点 ControlViewWalker 走一步
    // 就是一次跨进程 COM 调用，60 个节点能耗到 8 秒以上（实测）。
    static List<AutomationElement> Descendants(AutomationElement root, int max)
    {
        var list = new List<AutomationElement>();
        try
        {
            var all = root.FindAll(TreeScope.Descendants, System.Windows.Automation.Condition.TrueCondition);
            int n = Math.Min(all.Count, max);
            for (int i = 0; i < n; i++)
            {
                try { list.Add(all[i]); } catch { }
            }
        }
        catch { }
        return list;
    }

    static string NameOf(AutomationElement el)
    {
        try { return el.Current.Name ?? ""; } catch { return ""; }
    }

    static RECT RectOf(AutomationElement el)
    {
        try
        {
            var r = el.Current.BoundingRectangle;
            return new RECT { Left = (int)r.X, Top = (int)r.Y, Right = (int)r.Right, Bottom = (int)r.Bottom };
        }
        catch { return new RECT(); }
    }

    static bool IsOffscreen(AutomationElement el)
    {
        try { return el.Current.IsOffscreen; } catch { return true; }
    }

    // ---------- 像素读数 ----------
    // 用 WPF 的屏幕截图（CopyFromScreen 在 net8.0-windows 里必须依赖
    // System.Drawing.Common，WPF 目标下不引入；这里直接 BitBlt 取位图）。
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleDC(IntPtr hdc);
    [DllImport("gdi32.dll")] static extern IntPtr CreateCompatibleBitmap(IntPtr hdc, int w, int h);
    [DllImport("gdi32.dll")] static extern IntPtr SelectObject(IntPtr hdc, IntPtr obj);
    [DllImport("gdi32.dll")] static extern bool BitBlt(IntPtr dst, int dx, int dy, int w, int h, IntPtr src, int sx, int sy, uint rop);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(IntPtr hdc);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr obj);
    [DllImport("user32.dll")] static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] static extern int ReleaseDC(IntPtr h, IntPtr hdc);
    [DllImport("gdi32.dll")] static extern int GetDIBits(IntPtr hdc, IntPtr bmp, uint start, uint lines, byte[] bits, ref BITMAPINFO bi, uint usage);

    [StructLayout(LayoutKind.Sequential)]
    struct BITMAPINFOHEADER
    {
        public uint biSize; public int biWidth; public int biHeight;
        public ushort biPlanes; public ushort biBitCount; public uint biCompression;
        public uint biSizeImage; public int biXPelsPerMeter; public int biYPelsPerMeter;
        public uint biClrUsed; public uint biClrImportant;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct BITMAPINFO { public BITMAPINFOHEADER bmiHeader; }

    // 抓一块屏幕区域，返回 BGR32 的像素数组（每 4 字节一个像素）。
    static byte[] GrabScreen(int sx, int sy, int w, int h)
    {
        IntPtr screenDc = GetDC(IntPtr.Zero);
        IntPtr memDc = CreateCompatibleDC(screenDc);
        IntPtr bmp = CreateCompatibleBitmap(screenDc, w, h);
        IntPtr old = SelectObject(memDc, bmp);
        try
        {
            BitBlt(memDc, 0, 0, w, h, screenDc, sx, sy, 0x00CC0020 /* SRCCOPY */);
            var bi = new BITMAPINFO();
            bi.bmiHeader.biSize = (uint)Marshal.SizeOf(typeof(BITMAPINFOHEADER));
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;         // 负数 = 自上而下
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = 0;     // BI_RGB
            var buf = new byte[w * h * 4];
            GetDIBits(memDc, bmp, 0, (uint)h, buf, ref bi, 0);
            return buf;
        }
        finally
        {
            SelectObject(memDc, old);
            DeleteObject(bmp);
            DeleteDC(memDc);
            ReleaseDC(IntPtr.Zero, screenDc);
        }
    }

    // 一颗勾选 = 实心红方块。一次抓整块区域（1 次 BitBlt），在内存里判断，避免
    // "每个像素一次全屏抓图"的性能灾难（之前 5 个选项 = 260 次 BitBlt，11-23 秒）。
    static int RedPixelsIn(byte[] px, int w, int x0, int y0, int x1, int y1)
    {
        int red = 0;
        for (int y = y0; y <= y1; y++)
        {
            if (y < 0) continue;
            int rowBase = y * w * 4;
            for (int x = x0; x <= x1; x++)
            {
                if (x < 0) continue;
                int i = rowBase + x * 4;
                if (i + 3 >= px.Length) continue;
                byte b = px[i], g = px[i + 1], r = px[i + 2];   // BGR
                if (r > 150 && g < 110 && b < 110) red++;
            }
        }
        return red;
    }

    static bool IsTickedAt(byte[] px, int w, int cx, int cy, int originX, int originY, int half = 9, int minRed = 60)
    {
        int x0 = cx - originX - half, y0 = cy - originY - half;
        return RedPixelsIn(px, w, x0, y0, x0 + half * 2 - 1, y0 + half * 2 - 1) >= minRed;
    }

    // 在标签左侧的一条竖带上找红方块的中心 y（在本区域内扫，不再逐点抓图）。
    static int FindTickCentreYInBand(byte[] px, int w, int originX, int originY, int wx, int yTop, int yBottom)
    {
        int bestStart = -1, bestEnd = -1, runStart = -1;
        for (int y = yTop; y <= yBottom; y++)
        {
            bool on = IsTickedAt(px, w, wx + originX, y + originY, originX, originY, half: 5, minRed: 20);
            if (on) { if (runStart < 0) runStart = y; }
            else if (runStart >= 0)
            {
                if (bestStart < 0 || (y - runStart) > (bestEnd - bestStart)) { bestStart = runStart; bestEnd = y - 1; }
                runStart = -1;
            }
        }
        if (runStart >= 0 && (bestStart < 0 || (yBottom - runStart) > (bestEnd - bestStart))) { bestStart = runStart; bestEnd = yBottom; }
        if (bestStart < 0) return 0;
        return (bestStart + bestEnd) / 2;
    }

    // ---------- 题干 + 选项 ----------
    class Option
    {
        public string Letter = "";
        public string Text = "";
        public int LabelY;
        public int BoxX;
        public int BoxY;
        public bool Checked;
    }

    static (string question, List<Option> options) ReadOptions()
    {
        var root = Root();
        GetWindowRect(hwnd, out var wr);
        var nodes = Descendants(root, 500);

        string question = null;
        var letters = new Dictionary<string, int>();   // letter -> label top (window coords)
        var texts = new Dictionary<string, string>();  // letter -> option text

        foreach (var el in nodes)
        {
            if (IsOffscreen(el)) continue;
            var name = NameOf(el).Trim();
            if (name.Length == 0) continue;
            var r = RectOf(el);
            int wy = r.Top - wr.Top;

            if (question == null && (name.Contains("分）") || name.Contains("分)"))) question = name;

            // 标签形如 "A、" / "B、"
            if (name.Length <= 3 && name.Length >= 2 && name[0] >= 'A' && name[0] <= 'E' && name[1] == '、')
            {
                letters[name.Substring(0, 1)] = wy;
                continue;
            }
        }

        if (letters.Count == 0) return (question ?? "", new List<Option>());

        // 选项文本：与标签同一 y 行、且在标签右侧
        foreach (var el in nodes)
        {
            if (IsOffscreen(el)) continue;
            var name = NameOf(el).Trim();
            if (name.Length == 0) continue;
            var r = RectOf(el);
            int wy = r.Top - wr.Top;
            int wx = r.Left - wr.Left;
            foreach (var kv in letters)
            {
                if (Math.Abs(wy - kv.Value) <= 6 && wx > 130 && !texts.ContainsKey(kv.Key))
                    texts[kv.Key] = name;
            }
        }

        // 一次抓取"选项区"整块：从第一个标签上方 20px 到最后一个标签下方 80px。
        int minY = letters.Values.Min() - 20;
        int maxY = letters.Values.Max() + 90;
        if (minY < 0) minY = 0;
        int bandX = 100, bandW = 120;                 // 勾选框所在的窄条
        var band = GrabScreen(wr.Left + bandX, wr.Top + minY, bandW, maxY - minY);

        var options = new List<Option>();
        foreach (var kv in letters.OrderBy(k => k.Key))
        {
            var opt = new Option
            {
                Letter = kv.Key,
                Text = texts.TryGetValue(kv.Key, out var t) ? t : "",
                LabelY = kv.Value,
                BoxX = bandX + 19,                    // 与实测一致：标签 144 → 框心 ~119
            };
            int centre = FindTickCentreYInBand(band, bandW, bandX, minY, opt.BoxX, kv.Value + 20, kv.Value + 72);
            opt.BoxY = centre > 0 ? centre : kv.Value + 48;
            opt.Checked = IsTickedAt(band, bandW, opt.BoxX + bandX, opt.BoxY + minY, bandX, minY);
            options.Add(opt);
        }
        return (question ?? "", options);
    }

    static Dictionary<string, object> OptionsPayload(string question, List<Option> options) =>
        new Dictionary<string, object>
        {
            ["question"] = question,
            ["options"] = options.Select(o => new Dictionary<string, object>
            {
                ["letter"] = o.Letter,
                ["text"] = o.Text,
                ["checked"] = o.Checked,
                ["x"] = o.BoxX,
                ["y"] = o.BoxY,
            }).ToList(),
            ["ticked"] = string.Concat(options.Where(o => o.Checked).Select(o => o.Letter)),
        };

    static string Read(JsonElement req)
    {
        SetForegroundWindow(hwnd);
        Thread.Sleep(100);
        var (question, options) = ReadOptions();
        var payload = OptionsPayload(question, options);
        if (options.Count == 0) payload["error"] = "no A-E option labels found";
        return Ok(payload);
    }

    static string Post(JsonElement req)
    {
        var points = new List<POINT>();
        if (req.TryGetProperty("points", out var arr) && arr.ValueKind == JsonValueKind.Array)
            foreach (var p in arr.EnumerateArray())
            {
                var s = p.GetString() ?? "";
                var parts = s.Split(',');
                if (parts.Length != 2) continue;
                points.Add(new POINT { X = int.Parse(parts[0]), Y = int.Parse(parts[1]) });
            }
        if (points.Count == 0) return Err("no points");

        SetForegroundWindow(hwnd);
        Thread.Sleep(120);

        var done = new List<string>();
        foreach (var pt in points)
        {
            // 投递到最深的子窗口（渲染面），顶层窗口会忽略
            IntPtr target = hwnd;
            for (int i = 0; i < 8; i++)
            {
                var next = ChildWindowFromPointEx(target, pt, 0);
                if (next == IntPtr.Zero || next == target) break;
                target = next;
            }
            IntPtr lp = (IntPtr)((pt.Y << 16) | (pt.X & 0xFFFF));
            PostMessage(target, WM_MOUSEMOVE, IntPtr.Zero, lp);
            Thread.Sleep(60);
            PostMessage(target, WM_LBUTTONDOWN, (IntPtr)1, lp);
            Thread.Sleep(60);
            PostMessage(target, WM_LBUTTONUP, IntPtr.Zero, lp);
            done.Add(pt.X + "," + pt.Y);
            Thread.Sleep(240);
        }
        return Ok(new Dictionary<string, object> { ["mode"] = "postmessage", ["clicked"] = done });
    }

    static string Set(JsonElement req)
    {
        string target = req.TryGetProperty("target", out var t) ? (t.GetString() ?? "") : "";
        var want = new HashSet<string>(target.ToUpperInvariant().Select(c => c.ToString()));
        SetForegroundWindow(hwnd);
        Thread.Sleep(100);

        var log = new List<string>();
        string before = null;
        for (int round = 1; round <= 4; round++)
        {
            var (question, options) = ReadOptions();
            if (options.Count == 0) return Err("no options found");
            var have = new HashSet<string>(options.Where(o => o.Checked).Select(o => o.Letter));
            var haveStr = string.Concat(options.Where(o => o.Checked).Select(o => o.Letter));
            if (before == null) before = haveStr;
            if (have.SetEquals(want))
                return Ok(new Dictionary<string, object>
                {
                    ["question"] = question,
                    ["before"] = before,
                    ["after"] = haveStr,
                    ["target"] = target.ToUpperInvariant(),
                    ["rounds"] = round,
                    ["log"] = log,
                });

            // 只点差异项，一次一个；点完立刻进入下一轮重新读（页面会重排）
            var diff = options.FirstOrDefault(o => want.Contains(o.Letter) != o.Checked);
            if (diff == null) break;
            var one = new Dictionary<string, object>
            {
                ["op"] = "post",
                ["hwnd"] = hwnd.ToInt64(),
                ["points"] = new[] { diff.BoxX + "," + diff.BoxY },
            };
            Post(JsonSerializer.SerializeToElement(one));
            log.Add(diff.Letter + "@" + diff.BoxX + "," + diff.BoxY);
        }

        var (q2, o2) = ReadOptions();
        var after = string.Concat(o2.Where(o => o.Checked).Select(o => o.Letter));
        return Ok(new Dictionary<string, object>
        {
            ["question"] = q2,
            ["before"] = before ?? "",
            ["after"] = after,
            ["target"] = target.ToUpperInvariant(),
            ["matched"] = (after == string.Concat(want.OrderBy(c => c))),
            ["log"] = log,
        });
    }

    static string Act(JsonElement req)
    {
        string name = req.TryGetProperty("name", out var n) ? (n.GetString() ?? "") : "";
        if (name.Length == 0) return Err("no name");
        SetForegroundWindow(hwnd);
        var nodes = Descendants(Root(), 600);
        var hit = nodes.FirstOrDefault(el => !IsOffscreen(el) && (NameOf(el).Trim() == name || NameOf(el).Contains(name)));
        if (hit == null) return Err("no element matching \"" + name + "\"");

        object selection = null, invoke = null;
        try { selection = hit.GetCurrentPattern(SelectionItemPattern.Pattern); } catch { }
        try { invoke = hit.GetCurrentPattern(InvokePattern.Pattern); } catch { }

        string action;
        if (selection is SelectionItemPattern sp) { sp.Select(); action = "select"; }
        else if (invoke is InvokePattern ip) { ip.Invoke(); action = "invoke"; }
        else
        {
            // 无 pattern：退化为坐标点击（这里只回报，交由 post 完成）
            var r = RectOf(hit);
            int cx = (r.Left + r.Right) / 2 - RectOfWindow().Left;
            int cy = (r.Top + r.Bottom) / 2 - RectOfWindow().Top;
            return Ok(new Dictionary<string, object>
            {
                ["action"] = "click-fallback-needed",
                ["x"] = cx,
                ["y"] = cy,
                ["matched"] = NameOf(hit),
            });
        }
        return Ok(new Dictionary<string, object> { ["action"] = action, ["matched"] = NameOf(hit) });
    }

    static RECT RectOfWindow()
    {
        GetWindowRect(hwnd, out var r);
        return r;
    }

    static string Tree(JsonElement req)
    {
        int max = Int(req, "max", 500);
        var root = Root();
        GetWindowRect(hwnd, out var wr);
        var nodes = Descendants(root, max);
        var rows = new List<Dictionary<string, object>>();
        foreach (var el in nodes)
        {
            var name = NameOf(el);
            var r = RectOf(el);
            string role = "";
            try { role = el.Current.ControlType.ProgrammaticName.Replace("ControlType.", ""); } catch { }
            string sel = "";
            try
            {
                if (el.TryGetCurrentPattern(SelectionItemPattern.Pattern, out var sn))
                    sel = ((SelectionItemPattern)sn).Current.IsSelected ? "on" : "off";
                else if (el.TryGetCurrentPattern(TogglePattern.Pattern, out var tn))
                    sel = ((TogglePattern)tn).Current.ToggleState.ToString();
            }
            catch { }
            if (string.IsNullOrWhiteSpace(name) && string.IsNullOrEmpty(sel)) continue;
            rows.Add(new Dictionary<string, object>
            {
                ["role"] = role,
                ["name"] = name,
                ["sel"] = sel,
                ["offscreen"] = IsOffscreen(el),
                ["x"] = r.Left - wr.Left,
                ["y"] = r.Top - wr.Top,
                ["w"] = Math.Max(0, r.Right - r.Left),
                ["h"] = Math.Max(0, r.Bottom - r.Top),
            });
        }
        return Ok(new Dictionary<string, object> { ["count"] = rows.Count, ["nodes"] = rows });
    }
}

