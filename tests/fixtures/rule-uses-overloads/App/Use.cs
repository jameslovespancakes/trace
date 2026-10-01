using Demo.Model;

namespace Demo.App
{
    public class Use
    {
        public Node Copy(Node node)
        {
            return node.Clone();
        }

        public Node CopyWith(Node node, Settings settings)
        {
            return node.Clone(settings);
        }
    }
}
