// Minimal, self-checking WM_POINTER touch tapper.
//   dotnet run -- <hwnd> "x,y;x,y;..."
// Prints one JSON line. Exit 0 on success.
//
// Written because PowerShell cannot reliably build POINTER_TOUCH_INFO (value-type
// assignment silently leaves nested structs zeroed) and every wrong field surfaced only
// as InjectTouchInput -> FALSE, GetLastError=87.
using System;
using System.Runtime.InteropServices;
using System.Text;

static class TouchTap
{
    const int PT_TOUCH = 2;
    const uint FLAG_INRANGE = 0x00000002;
    const uint FLAG_INCONTACT = 0x00000004;
    const uint FLAG_DOWN = 0x00010000;
    const uint FLAG_UP = 0x00040000;
    const uint TOUCH_MASK_CONTACTAREA = 0x00000001;

    [StructLayout(LayoutKind.Sequential)]
    struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    struct POINTER_INFO
    {
        public int pointerType;
        public uint pointerId;
        public uint frameId;
        public uint pointerFlags;
        public IntPtr sourceDevice;
        public IntPtr hwndTarget;
        public int ptPixelLocationX, ptPixelLocationY;
        public int ptHimetricLocationX, ptHimetricLocationY;
        public int ptPixelLocationRawX, ptPixelLocationRawY;
        public int ptHimetricLocationRawX, ptHimetricLocationRawY;
        public uint dwTime;
        public uint historyCount;
        public int InputState;
        public uint dwKeyStates;
        public ulong PerformanceCount;
        public int ButtonChangeType;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct POINTER_TOUCH_INFO
    {
        public POINTER_INFO pointerInfo;
        public int touchFlags;
        public int touchMask;
        public RECT rcContact;
        public RECT rcContactRaw;
        public uint orientation;
        public uint pressure;
    }

    [DllImport("user32.dll", SetLastError = true)]
    static extern bool InitializeTouchInjection(uint maxCount, uint dwMode);

    [DllImport("user32.dll", SetLastError = true)]
    static extern bool InjectTouchInput(uint count, POINTER_TOUCH_INFO[] contacts);

    [DllImport("user32.dll")]
    static extern bool SetForegroundWindow(IntPtr hWnd);

    static int Main(string[] args)
    {
        if (args.Length < 2)
        {
            Console.WriteLine("{\"ok\":false,\"error\":\"usage: <hwnd> \\\"x,y;x,y\\\"\"}");
            return 1;
        }
        long hwndValue = long.Parse(args[0]);
        IntPtr hwnd = new IntPtr(hwndValue);

        int size = Marshal.SizeOf(typeof(POINTER_TOUCH_INFO));
        // POINTER_TOUCH_INFO is 144 bytes on x64; anything else means the marshalling is
        // wrong and every InjectTouchInput would fail with ERROR_INVALID_PARAMETER.
        if (size != 144)
        {
            Console.WriteLine("{\"ok\":false,\"error\":\"POINTER_TOUCH_INFO size " + size + " != 144\"}");
            return 2;
        }

        if (!InitializeTouchInjection(4, 1 /* TOUCH_FEEDBACK_DEFAULT */))
        {
            Console.WriteLine("{\"ok\":false,\"error\":\"InitializeTouchInjection err=" + Marshal.GetLastWin32Error() + "\"}");
            return 3;
        }
        SetForegroundWindow(hwnd);
        System.Threading.Thread.Sleep(150);

        var moved = new StringBuilder();
        uint id = 1;
        foreach (string piece in args[1].Split(';'))
        {
            if (piece.Trim().Length == 0) continue;
            string[] xy = piece.Split(',');
            int x = int.Parse(xy[0]);
            int y = int.Parse(xy[1]);

            var contact = new POINTER_TOUCH_INFO();
            contact.pointerInfo.pointerType = PT_TOUCH;
            contact.pointerInfo.pointerId = id;
            contact.pointerInfo.hwndTarget = hwnd;
            contact.touchFlags = 0;
            // rcContact is a LOCAL rect: (x-w/2, y-h/2, x+w/2, y+h/2). A 40x40 contact
            // area is what a fingertip actually covers; 4x4 was accepted by nothing.
            const int R = 20;
            contact.touchMask = (int)TOUCH_MASK_CONTACTAREA;
            contact.rcContact.Left = x - R;
            contact.rcContact.Top = y - R;
            contact.rcContact.Right = x + R;
            contact.rcContact.Bottom = y + R;
            contact.rcContactRaw = contact.rcContact;

            var down = contact;
            down.pointerInfo.pointerFlags = FLAG_DOWN | FLAG_INRANGE | FLAG_INCONTACT;
            bool okDown = InjectTouchInput(1, new[] { down });
            int errDown = Marshal.GetLastWin32Error();
            System.Threading.Thread.Sleep(60);

            var up = contact;
            up.pointerInfo.pointerFlags = FLAG_UP;
            bool okUp = InjectTouchInput(1, new[] { up });
            int errUp = Marshal.GetLastWin32Error();
            System.Threading.Thread.Sleep(220);

            if (moved.Length > 0) moved.Append(' ');
            moved.Append(x).Append(',').Append(y);
            if (!okDown || !okUp) moved.Append(":FAIL(").Append(errDown).Append('/').Append(errUp).Append(')');
            id++;
        }

        Console.WriteLine("{\"ok\":true,\"mode\":\"dotnet-touch\",\"size\":" + size + ",\"clicked\":\"" + moved + "\"}");
        return 0;
    }
}
