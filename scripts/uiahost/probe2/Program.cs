using System;
using System.Diagnostics;
using System.Windows.Automation;
class P {
  [STAThread]
  static void Main() {
    var sw = Stopwatch.StartNew();
    var el = AutomationElement.FromHandle((IntPtr)3801266);
    Console.WriteLine("FromHandle " + sw.ElapsedMilliseconds + "ms"); sw.Restart();
    Console.WriteLine("name=" + el.Current.Name + " " + sw.ElapsedMilliseconds + "ms"); sw.Restart();
    var all = el.FindAll(TreeScope.Descendants, Condition.TrueCondition);
    Console.WriteLine("FindAll=" + all.Count + " " + sw.ElapsedMilliseconds + "ms");
  }
}
