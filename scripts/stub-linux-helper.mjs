#!/usr/bin/env node
import readline from 'node:readline'

const LINUX_TOOLS = [
  {
    name: 'list_apps',
    description: 'List apps that can be targeted by the Linux window API.',
    parameters: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
  },
  {
    name: 'get_app_state',
    description: 'Capture the current state, screenshot, and accessibility text for an app window.',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App identifier or linux-window:<id>' },
        disableDiff: { type: 'boolean', description: 'Return full accessibility tree instead of diff' },
      },
      required: ['app'],
      additionalProperties: false,
    },
  },
  {
    name: 'screenshot',
    description: 'Capture a screenshot of an app window or the desktop.',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App identifier or linux-window:<id>' },
      },
      additionalProperties: false,
    },
  },
  {
    name: 'click',
    description: 'Click either an indexed element or a coordinate in the app window.',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App identifier or linux-window:<id>' },
        click_count: { type: 'number' },
        mouse_button: { type: 'string', enum: ['left', 'right', 'middle'] },
        x: { type: 'number' },
        y: { type: 'number' },
      },
      additionalProperties: false,
    },
  },
  {
    name: 'scroll',
    description: 'Scroll in an app window.',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App identifier or linux-window:<id>' },
        direction: { type: 'string', enum: ['up', 'down', 'left', 'right'] },
        pages: { type: 'number' },
        x: { type: 'number' },
        y: { type: 'number' },
      },
      required: ['direction'],
      additionalProperties: false,
    },
  },
  {
    name: 'press_key',
    description: 'Press a key or +-separated keyboard chord in an app window.',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App identifier or linux-window:<id>' },
        key: { type: 'string', description: 'Key or key chord (e.g. Return, Control_L+c)' },
      },
      required: ['key'],
      additionalProperties: false,
    },
  },
  {
    name: 'type_text',
    description: 'Type text into the current focus in an app window.',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App identifier or linux-window:<id>' },
        text: { type: 'string', description: 'Text to type' },
      },
      required: ['text'],
      additionalProperties: false,
    },
  },
]

const WINDOW2_TOOLS = [
  {
    name: 'list_windows',
    description: 'List open windows that can be targeted by the window2 API.',
    parameters: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
  },
  {
    name: 'get_window',
    description: 'Rehydrate a currently open window by id; useful after losing a window binding.',
    parameters: {
      type: 'object',
      properties: {
        id: { type: 'number', description: 'Opaque window identifier' },
        app: { type: 'string', description: 'Optional app identifier' },
      },
      required: ['id'],
      additionalProperties: false,
    },
  },
  {
    name: 'list_apps',
    description: 'List installed apps, including their currently open targetable windows when present.',
    parameters: {
      type: 'object',
      properties: {},
      additionalProperties: false,
    },
  },
  {
    name: 'launch_app',
    description: 'Launch an app by id so its window can be selected from list_apps().',
    parameters: {
      type: 'object',
      properties: {
        app: { type: 'string', description: 'App id or process name' },
      },
      required: ['app'],
      additionalProperties: false,
    },
  },
  {
    name: 'get_window_state',
    description: 'Capture selected state for an open window.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: {
            id: { type: 'number' },
            app: { type: 'string' },
            title: { type: 'string' },
          },
          required: ['id', 'app'],
        },
        include_screenshot: { type: 'boolean' },
        include_text: { type: 'boolean' },
      },
      required: ['window'],
      additionalProperties: false,
    },
  },
  {
    name: 'click',
    description: 'Click either an indexed element from the latest window state or a coordinate in the window.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
        },
        app: { type: 'string' },
        element_index: { type: 'number' },
        x: { type: 'number' },
        y: { type: 'number' },
        click_count: { type: 'number' },
        mouse_button: { type: 'string', enum: ['left', 'right', 'middle'] },
        screenshotId: { type: 'string' },
      },
      additionalProperties: false,
    },
  },
  {
    name: 'press_key',
    description: 'Press a +-separated keyboard chord in a window.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
        },
        app: { type: 'string' },
        key: { type: 'string' },
      },
      required: ['key'],
      additionalProperties: false,
    },
  },
  {
    name: 'type_text',
    description: 'Type text into the current focus in a window.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
        },
        app: { type: 'string' },
        text: { type: 'string' },
      },
      required: ['text'],
      additionalProperties: false,
    },
  },
  {
    name: 'scroll',
    description: 'Scroll by a delta from a specific coordinate in the window.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
        },
        app: { type: 'string' },
        direction: { type: 'string' },
        scrollX: { type: 'number' },
        scrollY: { type: 'number' },
        x: { type: 'number' },
        y: { type: 'number' },
        pages: { type: 'number' },
        screenshotId: { type: 'string' },
      },
      additionalProperties: false,
    },
  },
  {
    name: 'set_value',
    description: 'Replace the value of an indexed editable element.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
          required: ['id', 'app'],
        },
        element_index: { type: 'number' },
        value: { type: 'string' },
      },
      required: ['window', 'element_index', 'value'],
      additionalProperties: false,
    },
  },
  {
    name: 'drag',
    description: 'Drag from one window coordinate to another.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
          required: ['id', 'app'],
        },
        from_x: { type: 'number' },
        from_y: { type: 'number' },
        to_x: { type: 'number' },
        to_y: { type: 'number' },
        screenshotId: { type: 'string' },
      },
      required: ['window', 'from_x', 'from_y', 'to_x', 'to_y'],
      additionalProperties: false,
    },
  },
  {
    name: 'perform_secondary_action',
    description: 'Invoke a secondary accessibility action on an indexed element.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
          required: ['id', 'app'],
        },
        element_index: { type: 'number' },
        action: { type: 'string' },
      },
      required: ['window', 'element_index', 'action'],
      additionalProperties: false,
    },
  },
  {
    name: 'activate_window',
    description: 'Optional escape hatch to bring an open window to the foreground.',
    parameters: {
      type: 'object',
      properties: {
        window: {
          type: 'object',
          properties: { id: { type: 'number' }, app: { type: 'string' }, title: { type: 'string' } },
          required: ['id', 'app'],
        },
      },
      required: ['window'],
      additionalProperties: false,
    },
  },
]

// 1x1 transparent PNG base64
const DUMMY_PNG_BASE64 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=='

let currentSurface = process.env.DSH_COMPUTER_USE_SURFACE || 'linux'

/**
 * Whether a surface name denotes the window2 face, mirroring `isWindow2Surface` in
 * src/sidecar.js. A `call` carries `surface` so a name both faces define resolves to the
 * right handler; an untagged call keeps the P1 behaviour exactly.
 */
function isWindow2Surface(surface) {
  const name = String(surface || '').toLowerCase()
  return name === 'computer' || name === 'windows' || name === 'window2' || name === 'desktop'
}

const STUB_P1_ALLOWED = new Set(['list_apps', 'get_app_state', 'screenshot', 'click', 'scroll', 'press_key', 'type_text'])
// The window2 methods that do not take a window object: `get_window` takes an `id`, and
// these three take none at all. The other nine refuse a call without one.
const STUB_NO_WINDOW_NEEDED = new Set(['list_windows', 'list_apps', 'launch_app', 'get_window'])
// The eight window2 means the P1 surface does not define; they have no P1 handler, so the
// helper routes them natively whatever the call says.
const STUB_WINDOW2_ONLY = new Set([
  'list_windows',
  'get_window',
  'launch_app',
  'get_window_state',
  'set_value',
  'drag',
  'perform_secondary_action',
  'activate_window',
])

function handleCall(name, args = {}, callSurface = undefined) {
  // Mirrors `is_window2_native_call` in helper-linux/src/helper.rs: a window2-only name
  // always goes native, a name both faces define follows the call's own `surface` tag,
  // and an untagged call is a P1 call -- the helper carries no surface between requests.
  const window2 = STUB_WINDOW2_ONLY.has(name) || isWindow2Surface(callSurface)
  if (!window2 && !STUB_P1_ALLOWED.has(name)) {
    throw new Error(`unsupported method: ${name}`)
  }
  // Every window2 handler but these three takes a window object, and the real dispatcher
  // refuses the call before it looks at anything else. Mirroring that is what lets a test
  // tell "reached the window2 handler" apart from "was never routed there".
  if (window2 && !STUB_NO_WINDOW_NEEDED.has(name) && (!args.window || typeof args.window !== 'object')) {
    throw new Error('window is required and must be a Window object from list_windows()')
  }

  switch (name) {
    case 'list_windows':
      return {
        value: [
          { id: 101, app: 'org.gnome.TextEditor', title: 'Untitled Document' },
          { id: 102, app: 'linux-window:102', title: 'Terminal' },
        ],
        images: [],
      }
    case 'get_window':
      return {
        value: {
          id: args.id ?? 101,
          app: args.app || 'org.gnome.TextEditor',
          title: 'Untitled Document',
        },
        images: [],
      }
    case 'list_apps':
      if (window2) {
        return {
          value: [
            {
              id: 'org.gnome.TextEditor',
              displayName: 'Text Editor',
              isRunning: true,
              windows: [{ id: 101, app: 'org.gnome.TextEditor', title: 'Untitled Document' }],
            },
            {
              id: 'linux-window:102',
              displayName: 'Terminal',
              isRunning: true,
              windows: [{ id: 102, app: 'linux-window:102', title: 'Terminal' }],
            },
          ],
          images: [],
        }
      }
      return {
        value: [
          { id: 'org.gnome.TextEditor', displayName: 'Text Editor', isRunning: true },
          { id: 'linux-window:101', displayName: 'Terminal', isRunning: true },
        ],
        images: [],
      }
    case 'launch_app':
      return {
        value: { ok: true },
        images: [],
      }
    case 'get_window_state':
      return {
        value: {
          window: args.window || { id: 101, app: 'org.gnome.TextEditor', title: 'Untitled Document' },
          accessibility: args.include_text !== false ? {
            tree: 'Window: "Untitled Document", App: org.gnome.TextEditor\n1 edit [Document content]',
            focused_element: 1,
          } : null,
          screenshots: args.include_screenshot !== false ? [
            {
              id: 'shot-1',
              url: `data:image/png;base64,${DUMMY_PNG_BASE64}`,
              zIndex: 1,
              width: 1920,
              height: 1080,
            },
          ] : [],
        },
        images: args.include_screenshot !== false ? [
          {
            data: DUMMY_PNG_BASE64,
            mimeType: 'image/png',
            name: 'screenshot.png',
          },
        ] : [],
      }
    case 'set_value':
      return {
        value: {
          ok: true,
          action: 'set_value',
          element_index: args.element_index,
          value: args.value,
        },
        images: [],
      }
    case 'drag':
      return {
        value: {
          ok: true,
          action: 'drag',
          from_x: args.from_x,
          from_y: args.from_y,
          to_x: args.to_x,
          to_y: args.to_y,
        },
        images: [],
      }
    case 'perform_secondary_action':
      return {
        value: {
          ok: true,
          action: args.action,
          element_index: args.element_index,
        },
        images: [],
      }
    case 'activate_window':
      return {
        value: {
          ok: true,
          action: 'activate_window',
          window: args.window,
        },
        images: [],
      }
    case 'get_app_state':
      return {
        value: {
          app: args.app || 'org.gnome.TextEditor',
          text: 'Window: "Untitled Document", App: org.gnome.TextEditor\n1 edit [Document content]',
        },
        images: [
          {
            data: DUMMY_PNG_BASE64,
            mimeType: 'image/png',
            name: 'screenshot.png',
          },
        ],
      }
    case 'screenshot':
      return {
        value: {
          app: args.app || 'linux-window:101',
          width: 1920,
          height: 1080,
        },
        images: [
          {
            data: DUMMY_PNG_BASE64,
            mimeType: 'image/png',
            name: 'screenshot.png',
          },
        ],
      }
    case 'click':
      return {
        value: {
          ok: true,
          action: 'click',
          app: args.app || (args.window ? args.window.app : 'linux-window:101'),
          window: args.window,
          element_index: args.element_index,
          handler: window2 ? 'window2' : 'sky.window',
          x: args.x ?? 100,
          y: args.y ?? 200,
        },
        images: [],
      }
    case 'scroll':
      return {
        value: {
          ok: true,
          action: 'scroll',
          app: args.app || (args.window ? args.window.app : 'linux-window:101'),
          window: args.window,
          handler: window2 ? 'window2' : 'sky.window',
          direction: args.direction || 'down',
          scrollX: args.scrollX,
          scrollY: args.scrollY,
          pages: args.pages ?? 1,
        },
        images: [],
      }
    case 'press_key':
      return {
        value: {
          ok: true,
          action: 'press_key',
          app: args.app || (args.window ? args.window.app : 'linux-window:101'),
          window: args.window,
          handler: window2 ? 'window2' : 'sky.window',
          key: args.key || 'Return',
        },
        images: [],
      }
    case 'type_text':
      return {
        value: {
          ok: true,
          action: 'type_text',
          app: args.app || (args.window ? args.window.app : 'linux-window:101'),
          window: args.window,
          handler: window2 ? 'window2' : 'sky.window',
          text: args.text || '',
        },
        images: [],
      }
    default:
      throw new Error(`unsupported method: ${name}`)
  }
}

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
  terminal: false,
})

rl.on('line', line => {
  const text = line.trim()
  if (!text) return
  let req
  try {
    req = JSON.parse(text)
  } catch (err) {
    return
  }

  const { id, method, params } = req
  try {
    if (method === 'health') {
      console.log(JSON.stringify({
        id,
        ok: true,
        result: {
          ok: true,
          codexRequired: false,
          backend: 'linux',
          surface: currentSurface,
        },
      }))
      return
    }

    if (method === 'tools') {
      const reqSurface = params?.surface || currentSurface
      const isWindow2 = reqSurface === 'computer' || reqSurface === 'desktop'
      currentSurface = isWindow2 ? 'computer' : 'linux'
      console.log(JSON.stringify({
        id,
        ok: true,
        result: {
          tools: isWindow2 ? WINDOW2_TOOLS : LINUX_TOOLS,
          surface: currentSurface,
        },
      }))
      return
    }

    if (method === 'call') {
      const toolName = params?.name
      const toolArgs = params?.arguments || {}
      // A call may carry its own surface (the sidecar tags a window2 turn). An untagged
      // call is a P1 call; nothing is remembered from an earlier `tools` request.
      const res = handleCall(toolName, toolArgs, params?.surface)
      console.log(JSON.stringify({
        id,
        ok: true,
        result: {
          ok: true,
          name: toolName,
          value: res.value,
          images: res.images,
        },
      }))
      return
    }

    if (method === 'shutdown') {
      console.log(JSON.stringify({
        id,
        ok: true,
        result: { ok: true },
      }))
      setTimeout(() => process.exit(0), 10)
      return
    }

    if (method === 'end_turn' || method === 'interrupt' || method === 'cancel' || method === 'prompt') {
      console.log(JSON.stringify({
        id,
        ok: true,
        result: { ok: true },
      }))
      return
    }

    console.log(JSON.stringify({
      id,
      ok: false,
      error: `unsupported method: ${method}`,
    }))
  } catch (error) {
    console.log(JSON.stringify({
      id,
      ok: false,
      error: error.message || String(error),
    }))
  }
})
