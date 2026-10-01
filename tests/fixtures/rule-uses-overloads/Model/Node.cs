namespace Demo.Model
{
    public class Settings
    {
        public bool Deep { get; set; }
    }

    public class Node
    {
        public Node Clone()
        {
            return Clone(new Settings());
        }

        public Node Clone(Settings settings)
        {
            return new Node();
        }
    }
}
