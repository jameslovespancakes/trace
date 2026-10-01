use super::*;
use crate::test_support::fixture_assembly;

#[test]
fn rule_assembly_metadata_gives_bases_interfaces_and_loaded_literals() {
    let bytes = fixture_assembly();
    let a = read(&bytes).expect("CLI image");
    let read_attr = a.find("Lib.Web.ReadAttribute").expect("ReadAttribute");
    let base = read_attr.extends.as_ref().expect("base");
    assert_eq!(base.full(), "Lib.Web.Routing.VerbAttribute");
    assert_eq!(base.assembly, None, "defined in the same assembly");
    assert!(a.literals(&bytes, read_attr).contains(&"GET".to_string()));
    let verb = a.find("Lib.Web.Routing.VerbAttribute").expect("VerbAttribute");
    let interfaces: Vec<String> = verb.interfaces.iter().map(TypeName::full).collect();
    assert_eq!(
        interfaces,
        vec![
            "Lib.Web.Routing.ITemplateSource".to_string(),
            "Lib.Web.Routing.IVerbSource".to_string()
        ]
    );
    // A base in another assembly names that assembly.
    let plain = a.find("Lib.Web.PlainAttribute").expect("PlainAttribute");
    assert_eq!(plain.extends.as_ref().map(TypeName::full).as_deref(), Some("System.Attribute"));
    assert!(plain.extends.as_ref().and_then(|e| e.assembly.as_deref()).is_some());
    assert!(a.literals(&bytes, plain).is_empty());
}

#[test]
fn rule_il_instruction_sizes_follow_the_opcode_table() {
    // ldstr <token>; ret
    assert_eq!(instruction_size(&[0x72, 1, 0, 0, 0x70, 0x2A], 0), Some(5));
    assert_eq!(instruction_size(&[0x2A], 0), Some(1));
    // switch with two targets: opcode + count + 2 * 4.
    assert_eq!(instruction_size(&[0x45, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0), Some(13));
    // Two-byte opcode ldarg <u16>.
    assert_eq!(instruction_size(&[0xFE, 0x09, 1, 0], 0), Some(4));
    // Unknown opcodes stop the decoding.
    assert_eq!(instruction_size(&[0xA6], 0), None);
}

#[test]
fn rule_compressed_integers_follow_the_blob_encoding() {
    assert_eq!(compressed(&[0x03], 0), Some((3, 1)));
    assert_eq!(compressed(&[0x80, 0x80], 0), Some((0x80, 2)));
    assert_eq!(compressed(&[0xC0, 0x00, 0x40, 0x00], 0), Some((0x4000, 4)));
}

#[test]
fn rule_non_cli_images_have_no_types() {
    assert!(read(b"not a PE image").is_none());
    assert!(read(b"MZ").is_none());
}
