//! Type metadata of compiled .NET assemblies (ECMA-335 partition II): the
//! declarations an attribute chain needs when a package ships no source (DESIGN-bridges §2
//! rule 5, "C# attribute classes via metadata"). Read-only, bounded, no execution:
//!
//! * the PE / CLI headers locate the metadata root (II.24, II.25);
//! * the `#~` table stream gives every type definition with its namespace, name, base type
//!   (`Extends`), declared interfaces (`InterfaceImpl`) and methods (`MethodDef`), and the
//!   type references with the assembly they resolve to (`TypeRef` -> `AssemblyRef`);
//! * method bodies (II.25.4) are decoded instruction by instruction (III) to list the string
//!   literals a type's own code loads (`ldstr`, from the `#US` heap) - the values its
//!   constructors hand to the members the runtime reads.
//!
//! Anything malformed yields `None` / fewer facts, never a guess.

/// A type name as metadata spells it (`namespace` empty for the global namespace; nested
/// types carry their enclosing type's namespace).
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TypeName {
    pub namespace: String,
    pub name: String,
    /// Referenced assembly (`TypeRef` resolved through `AssemblyRef`), `None` for types
    /// defined in the reading assembly.
    pub assembly: Option<String>,
}

impl TypeName {
    /// `Namespace.Name`.
    pub fn full(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.namespace, self.name)
        }
    }
}

/// One type definition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TypeDef {
    pub namespace: String,
    pub name: String,
    pub extends: Option<TypeName>,
    pub interfaces: Vec<TypeName>,
    /// File offsets of the method bodies of the type's own methods (read lazily with
    /// [`Assembly::literals`]).
    pub bodies: Vec<u32>,
}

impl TypeDef {
    pub fn full(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.namespace, self.name)
        }
    }
}

/// The type definitions of one assembly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Assembly {
    pub types: Vec<TypeDef>,
    /// Types this assembly forwards to another one (`ExportedType` rows implemented by an
    /// `AssemblyRef`): `Namespace.Name` -> assembly name.
    pub forwards: Vec<(String, String)>,
    /// File range of the `#US` heap.
    user_strings: (usize, usize),
}

impl Assembly {
    pub fn find(&self, full: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.full() == full)
    }

    /// The assembly `full` is forwarded to.
    pub fn forwarded(&self, full: &str) -> Option<&str> {
        self.forwards.iter().find(|(n, _)| n == full).map(|(_, a)| a.as_str())
    }

    /// String literals the type's own methods load (`ldstr`), in method order; `file` is the
    /// assembly file's bytes this metadata was read from.
    pub fn literals(&self, file: &[u8], t: &TypeDef) -> Vec<String> {
        let heap = file
            .get(self.user_strings.0..self.user_strings.0 + self.user_strings.1)
            .unwrap_or(&[]);
        let mut out = Vec::new();
        for &at in &t.bodies {
            body_literals(heap, file, at as usize, &mut out);
        }
        out
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Compressed unsigned integer (II.23.2): (value, bytes used).
fn compressed(b: &[u8], o: usize) -> Option<(u32, usize)> {
    let b0 = *b.get(o)? as u32;
    if b0 & 0x80 == 0 {
        Some((b0, 1))
    } else if b0 & 0xC0 == 0x80 {
        Some((((b0 & 0x3F) << 8) | *b.get(o + 1)? as u32, 2))
    } else if b0 & 0xE0 == 0xC0 {
        let rest = b.get(o + 1..o + 4)?;
        Some((((b0 & 0x1F) << 24) | (rest[0] as u32) << 16 | (rest[1] as u32) << 8 | rest[2] as u32, 4))
    } else {
        None
    }
}

struct Sections {
    /// (virtual address, virtual size, raw pointer, raw size).
    list: Vec<(u32, u32, u32, u32)>,
}

impl Sections {
    fn offset(&self, rva: u32) -> Option<usize> {
        self.list.iter().find_map(|&(va, vs, ptr, rs)| {
            let size = vs.max(rs);
            (rva >= va && rva < va.checked_add(size)?).then(|| (ptr + (rva - va)) as usize)
        })
    }
}

// Table numbers (II.22).
const MODULE: usize = 0x00;
const TYPE_REF: usize = 0x01;
const TYPE_DEF: usize = 0x02;
const FIELD: usize = 0x04;
const METHOD_DEF: usize = 0x06;
const PARAM: usize = 0x08;
const INTERFACE_IMPL: usize = 0x09;
const MEMBER_REF: usize = 0x0A;
const DECL_SECURITY: usize = 0x0E;
const STAND_ALONE_SIG: usize = 0x11;
const EVENT: usize = 0x14;
const PROPERTY: usize = 0x17;
const MODULE_REF: usize = 0x1A;
const TYPE_SPEC: usize = 0x1B;
const ASSEMBLY: usize = 0x20;
const ASSEMBLY_REF: usize = 0x23;
const FILE: usize = 0x26;
const EXPORTED_TYPE: usize = 0x27;
const MANIFEST_RESOURCE: usize = 0x28;
const NESTED_CLASS: usize = 0x29;
const GENERIC_PARAM: usize = 0x2A;
const METHOD_SPEC: usize = 0x2B;
const GENERIC_PARAM_CONSTRAINT: usize = 0x2C;
const TABLES: usize = 0x2D;

/// Column kinds of the table schemas.
#[derive(Clone, Copy)]
enum Col {
    U8x2,
    U16,
    U32,
    Str,
    Guid,
    Blob,
    Table(usize),
    Coded(&'static [usize]),
}

// Coded index families (II.24.2.6); `usize::MAX` marks unused tags.
const NONE: usize = usize::MAX;
const TYPE_DEF_OR_REF: &[usize] = &[TYPE_DEF, TYPE_REF, TYPE_SPEC];
const HAS_CONSTANT: &[usize] = &[FIELD, PARAM, PROPERTY];
const HAS_CUSTOM_ATTRIBUTE: &[usize] = &[
    METHOD_DEF,
    FIELD,
    TYPE_REF,
    TYPE_DEF,
    PARAM,
    INTERFACE_IMPL,
    MEMBER_REF,
    MODULE,
    DECL_SECURITY,
    PROPERTY,
    EVENT,
    STAND_ALONE_SIG,
    MODULE_REF,
    TYPE_SPEC,
    ASSEMBLY,
    ASSEMBLY_REF,
    FILE,
    EXPORTED_TYPE,
    MANIFEST_RESOURCE,
    GENERIC_PARAM,
    GENERIC_PARAM_CONSTRAINT,
    METHOD_SPEC,
];
const HAS_FIELD_MARSHAL: &[usize] = &[FIELD, PARAM];
const HAS_DECL_SECURITY: &[usize] = &[TYPE_DEF, METHOD_DEF, ASSEMBLY];
const MEMBER_REF_PARENT: &[usize] = &[TYPE_DEF, TYPE_REF, MODULE_REF, METHOD_DEF, TYPE_SPEC];
const HAS_SEMANTICS: &[usize] = &[EVENT, PROPERTY];
const METHOD_DEF_OR_REF: &[usize] = &[METHOD_DEF, MEMBER_REF];
const MEMBER_FORWARDED: &[usize] = &[FIELD, METHOD_DEF];
const IMPLEMENTATION: &[usize] = &[FILE, ASSEMBLY_REF, EXPORTED_TYPE];
const CUSTOM_ATTRIBUTE_TYPE: &[usize] = &[NONE, NONE, METHOD_DEF, MEMBER_REF, NONE];
const RESOLUTION_SCOPE: &[usize] = &[MODULE, MODULE_REF, ASSEMBLY_REF, TYPE_REF];
const TYPE_OR_METHOD_DEF: &[usize] = &[TYPE_DEF, METHOD_DEF];

fn schema(table: usize) -> &'static [Col] {
    use Col::*;
    match table {
        0x00 => &[U16, Str, Guid, Guid, Guid],
        0x01 => &[Coded(RESOLUTION_SCOPE), Str, Str],
        0x02 => &[U32, Str, Str, Coded(TYPE_DEF_OR_REF), Table(FIELD), Table(METHOD_DEF)],
        0x03 => &[Table(FIELD)],
        0x04 => &[U16, Str, Blob],
        0x05 => &[Table(METHOD_DEF)],
        0x06 => &[U32, U16, U16, Str, Blob, Table(PARAM)],
        0x07 => &[Table(PARAM)],
        0x08 => &[U16, U16, Str],
        0x09 => &[Table(TYPE_DEF), Coded(TYPE_DEF_OR_REF)],
        0x0A => &[Coded(MEMBER_REF_PARENT), Str, Blob],
        0x0B => &[U8x2, Coded(HAS_CONSTANT), Blob],
        0x0C => &[Coded(HAS_CUSTOM_ATTRIBUTE), Coded(CUSTOM_ATTRIBUTE_TYPE), Blob],
        0x0D => &[Coded(HAS_FIELD_MARSHAL), Blob],
        0x0E => &[U16, Coded(HAS_DECL_SECURITY), Blob],
        0x0F => &[U16, U32, Table(TYPE_DEF)],
        0x10 => &[U32, Table(FIELD)],
        0x11 => &[Blob],
        0x12 => &[Table(TYPE_DEF), Table(EVENT)],
        0x13 => &[Table(EVENT)],
        0x14 => &[U16, Str, Coded(TYPE_DEF_OR_REF)],
        0x15 => &[Table(TYPE_DEF), Table(PROPERTY)],
        0x16 => &[Table(PROPERTY)],
        0x17 => &[U16, Str, Blob],
        0x18 => &[U16, Table(METHOD_DEF), Coded(HAS_SEMANTICS)],
        0x19 => &[Table(TYPE_DEF), Coded(METHOD_DEF_OR_REF), Coded(METHOD_DEF_OR_REF)],
        0x1A => &[Str],
        0x1B => &[Blob],
        0x1C => &[U16, Coded(MEMBER_FORWARDED), Str, Table(MODULE_REF)],
        0x1D => &[U32, Table(FIELD)],
        0x1E => &[U32, U32],
        0x1F => &[U32],
        0x20 => &[U32, U16, U16, U16, U16, U32, Blob, Str, Str],
        0x21 => &[U32],
        0x22 => &[U32, U32, U32],
        0x23 => &[U16, U16, U16, U16, U32, Blob, Str, Str, Blob],
        0x24 => &[U32, Table(ASSEMBLY_REF)],
        0x25 => &[U32, U32, U32, Table(ASSEMBLY_REF)],
        0x26 => &[U32, Str, Blob],
        0x27 => &[U32, U32, Str, Str, Coded(IMPLEMENTATION)],
        0x28 => &[U32, U32, Str, Coded(IMPLEMENTATION)],
        0x29 => &[Table(TYPE_DEF), Table(TYPE_DEF)],
        0x2A => &[U16, U16, Coded(TYPE_OR_METHOD_DEF), Str],
        0x2B => &[Coded(METHOD_DEF_OR_REF), Blob],
        0x2C => &[Table(GENERIC_PARAM), Coded(TYPE_DEF_OR_REF)],
        _ => &[],
    }
}

struct Tables<'b> {
    bytes: &'b [u8],
    rows: [u32; TABLES],
    start: [usize; TABLES],
    row_size: [usize; TABLES],
    str_wide: bool,
    guid_wide: bool,
    blob_wide: bool,
    strings: &'b [u8],
}

impl<'b> Tables<'b> {
    fn col_size(&self, c: Col) -> usize {
        match c {
            Col::U8x2 | Col::U16 => 2,
            Col::U32 => 4,
            Col::Str => 2 + 2 * usize::from(self.str_wide),
            Col::Guid => 2 + 2 * usize::from(self.guid_wide),
            Col::Blob => 2 + 2 * usize::from(self.blob_wide),
            Col::Table(t) => {
                if self.rows[t] < 0x10000 {
                    2
                } else {
                    4
                }
            }
            Col::Coded(family) => {
                let bits = (usize::BITS - (family.len() - 1).leading_zeros()) as usize;
                let max = family
                    .iter()
                    .filter(|&&t| t != NONE)
                    .map(|&t| self.rows[t])
                    .max()
                    .unwrap_or(0);
                if (max as u64) < (1u64 << (16 - bits)) {
                    2
                } else {
                    4
                }
            }
        }
    }

    /// Value of column `col` of 1-based row `row` of `table`.
    fn get(&self, table: usize, row: u32, col: usize) -> Option<u32> {
        if row == 0 || row > self.rows[table] {
            return None;
        }
        let cols = schema(table);
        let mut o = self.start[table] + (row as usize - 1) * self.row_size[table];
        for c in &cols[..col] {
            o += self.col_size(*c);
        }
        match self.col_size(*cols.get(col)?) {
            2 => u16_at(self.bytes, o).map(u32::from),
            _ => u32_at(self.bytes, o),
        }
    }

    fn string(&self, index: u32) -> String {
        let Some(rest) = self.strings.get(index as usize..) else {
            return String::new();
        };
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..end]).into_owned()
    }

    /// Decoded `TypeDefOrRef` coded index.
    fn type_def_or_ref(&self, coded: u32) -> Option<TypeName> {
        let (tag, row) = (coded & 3, coded >> 2);
        match tag {
            0 => Some(TypeName {
                namespace: self.string(self.get(TYPE_DEF, row, 2)?),
                name: self.string(self.get(TYPE_DEF, row, 1)?),
                assembly: None,
            }),
            1 => {
                let scope = self.get(TYPE_REF, row, 0)?;
                let assembly = match (scope & 3, scope >> 2) {
                    (2, r) => self.get(ASSEMBLY_REF, r, 6).map(|s| self.string(s)),
                    // A nested type reference: the enclosing type's assembly.
                    (3, r) => self
                        .get(TYPE_REF, r, 0)
                        .filter(|s| s & 3 == 2)
                        .and_then(|s| self.get(ASSEMBLY_REF, s >> 2, 6))
                        .map(|s| self.string(s)),
                    _ => None,
                };
                Some(TypeName {
                    namespace: self.string(self.get(TYPE_REF, row, 2)?),
                    name: self.string(self.get(TYPE_REF, row, 1)?),
                    assembly,
                })
            }
            // Generic instantiations (`TypeSpec`) are not followed.
            _ => None,
        }
    }
}

/// Operand size of an IL instruction at `code[i..]` (opcode bytes included), `None` for an
/// unknown opcode (decoding stops).
fn instruction_size(code: &[u8], i: usize) -> Option<usize> {
    let op = *code.get(i)?;
    let operand = match op {
        0x00..=0x0D
        | 0x14..=0x1E
        | 0x25
        | 0x26
        | 0x2A
        | 0x46..=0x6E
        | 0x76
        | 0x7A
        | 0x82..=0x8B
        | 0x8E
        | 0x90..=0xA2 => 0,
        0xB3..=0xBA | 0xC3 | 0xD1..=0xDC | 0xDF | 0xE0 => 0,
        0x0E..=0x13 | 0x1F | 0x2B..=0x37 | 0xDE => 1,
        0x20 | 0x22 | 0x27..=0x29 | 0x38..=0x44 | 0x6F..=0x75 | 0x79 | 0x7B..=0x81 | 0x8C | 0x8D | 0x8F => 4,
        0xA3..=0xA5 | 0xC2 | 0xC6 | 0xD0 | 0xDD => 4,
        0x21 | 0x23 => 8,
        0x45 => {
            let n = u32_at(code, i + 1)? as usize;
            4 + n.checked_mul(4)?
        }
        0xFE => {
            let op2 = *code.get(i + 1)?;
            let operand = match op2 {
                0x00..=0x05 | 0x0F | 0x11 | 0x13 | 0x14 | 0x17 | 0x18 | 0x1A | 0x1D | 0x1E => 0,
                0x12 | 0x19 => 1,
                0x09..=0x0E => 2,
                0x06 | 0x07 | 0x15 | 0x16 | 0x1C => 4,
                _ => return None,
            };
            return Some(2 + operand);
        }
        _ => return None,
    };
    Some(1 + operand)
}

/// The `#US` heap entry at `index` (II.24.2.4: length-prefixed UTF-16 plus a flag byte).
fn user_string(heap: &[u8], index: u32) -> Option<String> {
    let (len, used) = compressed(heap, index as usize)?;
    let start = index as usize + used;
    let bytes = heap.get(start..start + (len as usize).saturating_sub(1))?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16(&units).ok()
}

/// String literals `ldstr` loads in the method body at file offset `at`.
fn body_literals(heap: &[u8], file: &[u8], at: usize, out: &mut Vec<String>) {
    let Some(&first) = file.get(at) else { return };
    let (code_start, code_size) = match first & 3 {
        2 => (at + 1, (first >> 2) as usize),
        3 => {
            let Some(flags) = u16_at(file, at) else { return };
            let header = ((flags >> 12) as usize) * 4;
            let Some(size) = u32_at(file, at + 4) else { return };
            (at + header, size as usize)
        }
        _ => return,
    };
    let Some(code) = file.get(code_start..code_start.saturating_add(code_size)) else { return };
    let mut i = 0usize;
    while i < code.len() {
        let Some(n) = instruction_size(code, i) else { return };
        if code[i] == 0x72 {
            if let Some(token) = u32_at(code, i + 1) {
                if token >> 24 == 0x70 {
                    if let Some(s) = user_string(heap, token & 0x00FF_FFFF) {
                        out.push(s);
                    }
                }
            }
        }
        i += n;
    }
}

/// Type definitions of a .NET assembly file's bytes (`None`: not a CLI image).
pub fn read(file: &[u8]) -> Option<Assembly> {
    if file.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(file, 0x3C)? as usize;
    if file.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let coff = pe + 4;
    let sections_n = u16_at(file, coff + 2)? as usize;
    let opt_size = u16_at(file, coff + 16)? as usize;
    let opt = coff + 20;
    let dirs = match u16_at(file, opt)? {
        0x10b => opt + 96,
        0x20b => opt + 112,
        _ => return None,
    };
    let cli_rva = u32_at(file, dirs + 14 * 8)?;
    let mut sections = Sections { list: Vec::new() };
    let table = opt + opt_size;
    for s in 0..sections_n.min(96) {
        let o = table + s * 40;
        sections.list.push((
            u32_at(file, o + 12)?,
            u32_at(file, o + 8)?,
            u32_at(file, o + 20)?,
            u32_at(file, o + 16)?,
        ));
    }
    let cli = sections.offset(cli_rva)?;
    let meta = sections.offset(u32_at(file, cli + 8)?)?;
    if u32_at(file, meta)? != 0x424A_5342 {
        return None;
    }
    let version_len = u32_at(file, meta + 12)? as usize;
    let streams_n = u16_at(file, meta + 16 + version_len + 2)? as usize;
    let mut o = meta + 16 + version_len + 4;
    let (mut tilde, mut strings, mut user_strings) = (None, &[][..], (0usize, 0usize));
    for _ in 0..streams_n.min(16) {
        let off = u32_at(file, o)? as usize;
        let size = u32_at(file, o + 4)? as usize;
        let name_start = o + 8;
        let name_len = file.get(name_start..)?.iter().position(|&b| b == 0)?;
        let name = file.get(name_start..name_start + name_len)?;
        let data = file.get(meta + off..meta + off + size)?;
        match name {
            b"#~" => tilde = Some(data),
            b"#Strings" => strings = data,
            b"#US" => user_strings = (meta + off, size),
            _ => {}
        }
        o = name_start + ((name_len + 4) & !3);
    }
    let tilde = tilde?;
    let heap = *tilde.get(6)?;
    let valid = u64::from_le_bytes(tilde.get(8..16)?.try_into().ok()?);
    let mut rows = [0u32; TABLES];
    let mut p = 24usize;
    for (t, r) in rows.iter_mut().enumerate() {
        if valid & (1u64 << t) != 0 {
            *r = u32_at(tilde, p)?;
            p += 4;
        }
    }
    // Tables beyond the known ones would shift every offset: not readable.
    if valid >> TABLES != 0 {
        return None;
    }
    let mut t = Tables {
        bytes: tilde,
        rows,
        start: [0; TABLES],
        row_size: [0; TABLES],
        str_wide: heap & 1 != 0,
        guid_wide: heap & 2 != 0,
        blob_wide: heap & 4 != 0,
        strings,
    };
    for table in 0..TABLES {
        let size: usize = schema(table).iter().map(|c| t.col_size(*c)).sum();
        t.row_size[table] = size;
        t.start[table] = p;
        p += size * t.rows[table] as usize;
    }
    if p > tilde.len() {
        return None;
    }
    let type_count = t.rows[TYPE_DEF];
    let method_count = t.rows[METHOD_DEF];
    let mut types = Vec::with_capacity(type_count as usize);
    for row in 1..=type_count {
        let extends = t
            .get(TYPE_DEF, row, 3)
            .filter(|&c| c >> 2 != 0)
            .and_then(|c| t.type_def_or_ref(c));
        let first = t.get(TYPE_DEF, row, 5)?;
        let end = if row < type_count {
            t.get(TYPE_DEF, row + 1, 5)?
        } else {
            method_count + 1
        };
        let mut bodies = Vec::new();
        for m in first..end.min(method_count + 1) {
            let rva = t.get(METHOD_DEF, m, 0).unwrap_or(0);
            if rva == 0 {
                continue;
            }
            if let Some(at) = sections.offset(rva) {
                bodies.push(at as u32);
            }
        }
        types.push(TypeDef {
            namespace: t.string(t.get(TYPE_DEF, row, 2)?),
            name: t.string(t.get(TYPE_DEF, row, 1)?),
            extends,
            interfaces: Vec::new(),
            bodies,
        });
    }
    for row in 1..=t.rows[INTERFACE_IMPL] {
        let class = t.get(INTERFACE_IMPL, row, 0)?;
        let Some(iface) = t.get(INTERFACE_IMPL, row, 1).and_then(|c| t.type_def_or_ref(c)) else {
            continue;
        };
        if let Some(ty) = types.get_mut((class as usize).wrapping_sub(1)) {
            ty.interfaces.push(iface);
        }
    }
    // Nested types keep their enclosing type's namespace (`Outer/Inner` spelled `Inner`).
    for row in 1..=t.rows[NESTED_CLASS] {
        let (Some(nested), Some(outer)) = (t.get(NESTED_CLASS, row, 0), t.get(NESTED_CLASS, row, 1)) else {
            continue;
        };
        let ns = types
            .get((outer as usize).wrapping_sub(1))
            .map(|o| o.namespace.clone());
        if let (Some(ns), Some(n)) = (ns, types.get_mut((nested as usize).wrapping_sub(1))) {
            if n.namespace.is_empty() {
                n.namespace = ns;
            }
        }
    }
    let mut forwards = Vec::new();
    for row in 1..=t.rows[EXPORTED_TYPE] {
        let Some(implementation) = t.get(EXPORTED_TYPE, row, 4) else { continue };
        if implementation & 3 != 1 {
            continue;
        }
        let (Some(name), Some(namespace), Some(assembly)) = (
            t.get(EXPORTED_TYPE, row, 2),
            t.get(EXPORTED_TYPE, row, 3),
            t.get(ASSEMBLY_REF, implementation >> 2, 6),
        ) else {
            continue;
        };
        let (name, namespace) = (t.string(name), t.string(namespace));
        let full = if namespace.is_empty() {
            name
        } else {
            format!("{namespace}.{name}")
        };
        forwards.push((full, t.string(assembly)));
    }
    Some(Assembly {
        types,
        forwards,
        user_strings,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/reflect/clr_meta.rs"]
mod tests;
