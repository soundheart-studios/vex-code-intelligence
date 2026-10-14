using System;
using Alias = System.Text;

namespace Outer.Inner;

[Serializable]
public struct Point { public int X { get; set; } }

#if DEBUG
public class Debugging {
    public void Trace() { Console.WriteLine(Helper.Format("x")); }
}
#elif RELEASE
public class Releasing { public void Run() { new Point(); } }
#else
public interface IOther { void Go(); }
#endif

public enum Color { Red, Green }
