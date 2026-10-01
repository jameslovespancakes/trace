//! Language rules of names, modules, inheritance, dispatch and types that inference
//! (trace-infer: family edges, value flow, candidate narrowing, receiver types) and the
//! semantic engine (trace-semantic: calls answered by syntax, blind sites) apply.
//! Data only: one [`LanguageRules`] per language ([`crate::spec::SyntaxSpec::rules`], set in
//! each `crate::languages` file); the engines read these flags and never match on the
//! language.

use trace_core::Language;

/// Name, module and type rules of one language (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct LanguageRules {
    // ---- inheritance and families ------------------------------------------------------
    /// Provider rule name of family edges (`rule:<name>`); empty = `<language>-inheritance`.
    pub inheritance_rule: &'static str,
    /// Types conform implicitly (structurally): no declared bases, no family edges (Go).
    pub implicit_conformance: bool,
    /// An override's base member is the next definer along the linearized MRO (Python C3),
    /// not every nearest definer along each base path.
    pub overrides_follow_mro: bool,
    /// Methods implement traits only through out-of-line `impl Trait for Type` relations
    /// (relation members only, `implements` edges); trait-typed receivers dispatch to the
    /// impls of the trait (Rust).
    pub trait_impls: bool,
    /// Constructor declared as an ordinary method of this name (`__init__`, `constructor`).
    pub named_constructor: Option<&'static str>,
    /// How bodiless signature declarations link to their definitions.
    pub signatures: Signatures,
    /// Provider rule name of naming-convention methods: a function `g.<class>` implements
    /// the generic `g` (a stub by `SyntaxSpec::generic_dispatch_calls`); empty = none.
    pub generic_method_rule: &'static str,
    /// Interfaces may be mixins whose uses the syntax does not record (PHP traits).
    pub interfaces_may_be_mixins: bool,

    // ---- value flow --------------------------------------------------------------------
    /// The super keyword as a receiver and the separator before the member (`super.m()`,
    /// C# `base.m()`, PHP `parent::m()`).
    pub super_receiver: Option<(&'static str, &'static str)>,
    /// Method a call of an instance runs in the value flow (`__call__`).
    pub instance_call_method: Option<&'static str>,
    /// Hook intercepting every attribute access (`__getattribute__`): members of library
    /// bases never apply to a class declaring it.
    pub attribute_hook: Option<&'static str>,
    /// Wrapper types whose method calls auto-dereference to the wrapped value.
    pub deref_wrappers: &'static [&'static str],
    /// CommonJS module values: `require("./x")` loads a file's `module.exports` value.
    pub commonjs_modules: bool,
    /// A read the scoping facts prove local reads the nearest enclosing function scope that
    /// binds the name, never the module-level variable.
    pub lexical_local_reads: bool,

    // ---- names and modules -------------------------------------------------------------
    /// How import targets name module files.
    pub modules: ModulePaths,
    /// Stem suffix of declaration files answering the plain specifier (`m.d.ts` for `./m`).
    pub declaration_file_suffix: &'static str,
    /// Whether bare calls reach methods.
    pub bare_calls: BareCalls,
    /// How far a bare name reaches free functions of other files.
    pub bare_reach: NameReach,
    /// A parameter / local shadows a callable of the same name.
    pub locals_shadow_functions: bool,
    /// Nested named functions are global once defined (not lexically scoped).
    pub global_nested_functions: bool,
    /// A receiver that is an allocation (`new K().m()`, `K{..}.m()`) names the run-time
    /// class (or the method set of the named type): method candidates narrow to it.
    pub allocation_receivers: bool,
    /// Static imports of members may bind a bare name (an unresolved import may bind any).
    pub static_imports: bool,
    /// Explicit single-name imports bind the simple name.
    pub single_name_imports: bool,
    /// Decorators / attribute macros may rewrite a function's parameter list.
    pub decorators_rewrite_signatures: bool,
    /// Bodiless free functions are foreign prototypes (their parameter lists are not the
    /// ones a call binds).
    pub foreign_prototypes: bool,
    /// How a bare call binds to a function declared in the same file when syntax alone
    /// answers it (SPEC section 8.8 rule 2); `None`: the language server answers.
    pub bare_call_binding: Option<BareCallBinding>,
    /// A function declared inside a function body is visible to bare names in that body
    /// only.
    pub nested_functions_are_local: bool,
    /// Calls inside templates may depend on template parameters (resolved per
    /// instantiation, never by the template's own definition).
    pub template_dependent_calls: bool,

    // ---- types -------------------------------------------------------------------------
    /// The compiler enforces declared types: a syntax type of the receiver proves which
    /// implementation runs.
    pub static_types: bool,
    /// Type compatibility is structural: an object of an unrelated declared type may still
    /// provide the member.
    pub structural_typing: bool,
    /// Receivers forward member calls through dereference (`Deref`, `->`).
    pub deref_forwarding: bool,
    /// A bare name inside a type body reads a field of the implicit receiver.
    pub implicit_field_receiver: bool,
    /// Calling a type's name constructs an instance (`Foo()`).
    pub calls_construct: bool,
    /// Files are grouped in packages by directory (package-private names are visible
    /// without an import).
    pub package_private_by_directory: bool,
}

/// How bodiless signature declarations link to their definitions (`stub_implementation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signatures {
    /// No signature rule.
    None,
    /// Every equation of the binding (one binding, one equation per clause).
    EveryEquation,
    /// The unique implementation with the same qualified name in the same file.
    UniqueImplementation,
}

/// Module path rules of import targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModulePaths {
    /// Dotted / path targets by longest stem suffix only (includes, namespaces).
    Global,
    /// `source` / `.` paths relative to the sourcing script, else by literal tail.
    SourcedFiles,
    /// Relative specifiers (`./m`); `export *` re-exports; package names are libraries.
    RelativeSpecifiers,
    /// Dotted modules with relative dots; module-level member imports re-export.
    DottedModules,
    /// Import paths naming package directories by suffix.
    ImportPaths,
    /// `crate::` / `self::` / `super::` / workspace crate / child-module paths.
    CratePaths,
    /// Dotted packages mapped to directories.
    PackageDirectories,
    /// `\`-separated namespaces mapped to directories after a vendor prefix (PSR-4).
    NamespaceDirectories,
    /// Dotted module names mapped to module files.
    ModuleFiles,
}

impl ModulePaths {
    /// Packages / namespaces map to directories (the directory fallback of module paths).
    pub fn packages_are_directories(self) -> bool {
        matches!(self, Self::PackageDirectories | Self::NamespaceDirectories | Self::ModuleFiles)
    }

    /// Separator of package / namespace segments.
    pub fn namespace_separator(self) -> char {
        if self == Self::NamespaceDirectories {
            '\\'
        } else {
            '.'
        }
    }
}

/// Whether bare calls reach methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareCalls {
    /// No rule.
    Unknown,
    /// Never: bare calls reach free functions only.
    FunctionsOnly,
    /// Through the implicit receiver (the class family of an enclosing class).
    ImplicitReceiver,
}

/// How a bare call name binds to a function declared in the same file
/// ([`LanguageRules::bare_call_binding`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareCallBinding {
    /// Shell functions: one global function namespace; a call names the function defined
    /// anywhere in the sourced scripts (the only declaration of the name is the answer).
    Shell,
    /// File-level functions (top level, unqualified). `declared_before`: the binding is only
    /// visible after its declaration (R assignments), so the declaration must precede the
    /// call.
    FileLevel { declared_before: bool },
}

/// How far a bare name reaches free functions of other files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameReach {
    /// A global namespace (C symbols, PHP / R / Bash globals, Java packages ...).
    Global,
    /// Same package (directory).
    Package,
    /// Same file or an import binding the name.
    FileOrImports,
}

/// No rule (languages without a grammar; the base of every language's rules).
pub const NONE: LanguageRules = LanguageRules {
    inheritance_rule: "",
    implicit_conformance: false,
    overrides_follow_mro: false,
    trait_impls: false,
    named_constructor: None,
    signatures: Signatures::None,
    generic_method_rule: "",
    interfaces_may_be_mixins: false,
    super_receiver: None,
    instance_call_method: None,
    attribute_hook: None,
    deref_wrappers: &[],
    commonjs_modules: false,
    lexical_local_reads: false,
    modules: ModulePaths::Global,
    declaration_file_suffix: "",
    bare_calls: BareCalls::Unknown,
    bare_reach: NameReach::Global,
    locals_shadow_functions: false,
    global_nested_functions: false,
    allocation_receivers: false,
    static_imports: false,
    single_name_imports: false,
    decorators_rewrite_signatures: false,
    foreign_prototypes: false,
    bare_call_binding: None,
    nested_functions_are_local: false,
    template_dependent_calls: false,
    static_types: false,
    structural_typing: false,
    deref_forwarding: false,
    implicit_field_receiver: false,
    calls_construct: false,
    package_private_by_directory: false,
};

/// The rules of `language` ([`NONE`] for languages without a grammar).
pub fn rules(language: Language) -> &'static LanguageRules {
    crate::languages::syntax(language).map_or(&NONE, |s| &s.rules)
}
