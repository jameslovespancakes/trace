// Fixture (bridges gate, csharp subprocess and ffi families).
using System.Diagnostics;
using System.Runtime.InteropServices;

public static class Native
{
    [DllImport("native", EntryPoint = "compress_buf")]
    public static extern int Compress(int n);

    public static Process Build() => Process.Start("python", "procs/build.py");
}
