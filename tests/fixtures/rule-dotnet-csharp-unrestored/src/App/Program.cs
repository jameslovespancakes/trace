namespace App;

public static class Program
{
    public static int Main() => Greeter.Count("trace");
}

public static class Greeter
{
    public static int Count(string text) => text.Length;
}
