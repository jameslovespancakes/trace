// Fixture (rule-derive-clr-metadata): attribute classes whose lineage reaches an interface.
using System;
using System.Collections.Generic;

namespace Lib.Web.Routing
{
    public interface ITemplateSource { string Template { get; } }
    public interface IVerbSource { IEnumerable<string> Verbs { get; } }

    public abstract class VerbAttribute : Attribute, ITemplateSource, IVerbSource
    {
        protected VerbAttribute(IEnumerable<string> verbs, string template) { Verbs = verbs; Template = template; }
        public IEnumerable<string> Verbs { get; }
        public string Template { get; }
    }
}

namespace Lib.Web
{
    public sealed class ReadAttribute : Routing.VerbAttribute
    {
        private static readonly string[] Supported = new[] { "GET" };
        public ReadAttribute(string template) : base(Supported, template) { }
    }

    public sealed class PlainAttribute : Attribute { }
}
