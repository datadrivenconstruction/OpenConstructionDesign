//! Reads the generated attribute table (`schema_table.rs`): attribute lists per class and
//! schema, and the check of one attribute value against its declared type.

use super::reader::Value;
use super::schema_table::CLASSES;

/// Declared type of an attribute, reduced to what a STEP value shows.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Kind {
    Ref,
    Int,
    Real,
    Number,
    Str,
    Binary,
    Enum(Vec<&'static str>),
    /// SELECT: a reference or a typed value, not checked further.
    Select,
    List(Box<Kind>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Attr {
    pub optional: bool,
    /// DERIVE override: the file must hold `*`.
    pub derived: bool,
    pub kind: Kind,
}

/// The IFC versions the product writes and checks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IfcSchema {
    #[default]
    Ifc4,
    Ifc2x3,
    Ifc4x3,
}

impl IfcSchema {
    /// Name in FILE_SCHEMA.
    pub fn header_name(self) -> &'static str {
        match self {
            IfcSchema::Ifc4 => "IFC4",
            IfcSchema::Ifc2x3 => "IFC2X3",
            IfcSchema::Ifc4x3 => "IFC4X3_ADD2",
        }
    }

    /// The version of a FILE_SCHEMA name written by this product.
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Ifc2x3, Self::Ifc4, Self::Ifc4x3]
            .into_iter()
            .find(|s| s.header_name() == name)
    }

    /// The version family of any FILE_SCHEMA name (`IFC4X3_RC4`, `IFC4X3` and so on).
    pub(super) fn family(name: &str) -> Option<Self> {
        if name.starts_with("IFC2X3") {
            Some(Self::Ifc2x3)
        } else if name.starts_with("IFC4X3") {
            Some(Self::Ifc4x3)
        } else if name.starts_with("IFC4") {
            Some(Self::Ifc4)
        } else {
            None
        }
    }

    /// Parses a user's choice: `IFC2X3`, `IFC4`, `IFC4X3` (any case, `_ADD2` and the like allowed).
    pub fn from_user(s: &str) -> Option<Self> {
        Self::family(&s.trim().to_ascii_uppercase())
    }

    /// The version family named in the FILE_SCHEMA of a STEP text header; `None` without one.
    pub fn of_header(text: &str) -> Option<Self> {
        let at = text.find("FILE_SCHEMA")?;
        let rest = &text[at..];
        let open = rest.find('\'')?;
        let name = &rest[open + 1..];
        let close = name.find('\'')?;
        Self::family(&name[..close].trim().to_ascii_uppercase())
    }

    /// The schema family of a `.ifc` file on disk (header only); `None` for other files.
    pub fn of_file(path: &std::path::Path) -> Option<Self> {
        use std::io::Read;
        let mut head = Vec::new();
        let f = std::fs::File::open(path).ok()?;
        f.take(64 * 1024).read_to_end(&mut head).ok()?;
        Self::of_header(&String::from_utf8_lossy(&head))
    }

    /// Column of the generated attribute table.
    pub(super) fn column(self) -> usize {
        match self {
            IfcSchema::Ifc2x3 => 0,
            IfcSchema::Ifc4 => 1,
            IfcSchema::Ifc4x3 => 2,
        }
    }
}

fn kind(s: &'static str) -> Option<Kind> {
    if let Some(inner) = s.strip_prefix("l(").and_then(|r| r.strip_suffix(')')) {
        return Some(Kind::List(Box::new(kind(inner)?)));
    }
    if let Some(vals) = s.strip_prefix("e=") {
        return Some(Kind::Enum(vals.split('|').collect()));
    }
    Some(match s {
        "r" => Kind::Ref,
        "i" => Kind::Int,
        "n" => Kind::Real,
        "u" => Kind::Number,
        "s" => Kind::Str,
        "y" => Kind::Binary,
        "x" => Kind::Select,
        _ => return None,
    })
}

/// Attributes of one spec string of the table.
pub(super) fn parse(spec: &'static str) -> Option<Vec<Attr>> {
    spec.split(';')
        .map(|a| {
            if a == "*" {
                return Some(Attr {
                    optional: false,
                    derived: true,
                    kind: Kind::Select,
                });
            }
            let (optional, k) = match a.strip_prefix('?') {
                Some(k) => (true, k),
                None => (false, a),
            };
            Some(Attr {
                optional,
                derived: false,
                kind: kind(k)?,
            })
        })
        .collect()
}

/// Attributes of `class` in schema column `col`: `None` when the table does not list the class,
/// `Some(None)` when the schema has no such entity.
pub(super) fn attrs(class: &str, col: usize) -> Option<Option<Vec<Attr>>> {
    let i = CLASSES.binary_search_by(|(c, _)| (*c).cmp(class)).ok()?;
    Some(CLASSES[i].1[col].and_then(parse))
}

fn is_ok(kind: &Kind, v: &Value) -> bool {
    match (kind, v) {
        (Kind::Ref, Value::Ref(_)) => true,
        (Kind::Int, Value::Int(_)) => true,
        (Kind::Real, Value::Real(_)) => true,
        (Kind::Number, Value::Int(_) | Value::Real(_)) => true,
        (Kind::Str, Value::Str(_)) => true,
        (Kind::Binary, Value::Binary(_)) => true,
        (Kind::Enum(vals), Value::Enum(e)) => vals.contains(&e.as_str()),
        (Kind::Select, Value::Null | Value::Derived) => false,
        (Kind::Select, _) => true,
        (Kind::List(inner), Value::List(items)) => items.iter().all(|x| is_ok(inner, x)),
        _ => false,
    }
}

fn shown(v: &Value) -> &'static str {
    match v {
        Value::Null => "$",
        Value::Derived => "*",
        Value::Int(_) => "an integer",
        Value::Real(_) => "a real",
        Value::Str(_) => "a string",
        Value::Enum(_) => "an enumeration value",
        Value::Ref(_) => "a reference",
        Value::List(_) => "a list",
        Value::Typed(..) => "a typed value",
        Value::Binary(_) => "a binary",
    }
}

/// `Err(why)` when `v` is not a valid value of attribute `a`.
pub(super) fn check(a: &Attr, v: &Value) -> Result<(), String> {
    match v {
        Value::Derived if a.derived => Ok(()),
        Value::Derived => Err("`*` where the attribute is not derived".into()),
        _ if a.derived => Err(format!("{} where the attribute is derived (`*`)", shown(v))),
        Value::Null if a.optional => Ok(()),
        Value::Null => Err("`$` in a required attribute".into()),
        _ if is_ok(&a.kind, v) => Ok(()),
        _ => Err(format!("{} where {:?} is declared", shown(v), a.kind)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(class: &str, col: usize) -> Option<usize> {
        attrs(class, col)?.map(|a| a.len())
    }

    #[test]
    fn the_table_is_sorted_and_every_spec_parses() {
        assert!(CLASSES.windows(2).all(|w| w[0].0 < w[1].0), "binary search needs order");
        for (name, cols) in CLASSES {
            for (i, c) in cols.iter().enumerate() {
                if let Some(s) = c {
                    assert!(parse(s).is_some(), "{name} column {i}: {s}");
                }
            }
        }
    }

    #[test]
    fn counts_follow_the_express_definitions() {
        assert_eq!(count("IFCPROJECT", 0), Some(9));
        assert_eq!(count("IFCPROJECT", 1), Some(9));
        assert_eq!(count("IFCGRID", 0), Some(10));
        assert_eq!(count("IFCGRID", 1), Some(11));
        assert_eq!(count("IFCWALL", 0), Some(8));
        assert_eq!(count("IFCWALL", 1), Some(9));
        assert_eq!(count("IFCDOOR", 0), Some(10));
        assert_eq!(count("IFCDOOR", 1), Some(13));
        assert_eq!(count("IFCSIUNIT", 1), Some(4));
        // classes that exist in one schema only
        assert_eq!(count("IFCDOORSTYLE", 0), Some(12));
        assert_eq!(count("IFCDOORSTYLE", 1), Some(12));
        assert_eq!(count("IFCDOORTYPE", 0), None);
        assert_eq!(count("IFCDOORTYPE", 1), Some(13));
        assert!(attrs("IFCNOSUCHTHING", 1).is_none());
    }

    #[test]
    fn attribute_types_come_from_the_declarations() {
        let wall = attrs("IFCWALL", 1).unwrap().unwrap();
        assert_eq!(wall[0].kind, Kind::Str);
        assert!(!wall[0].optional && wall[1].optional);
        assert!(matches!(&wall[8].kind, Kind::Enum(v) if v.contains(&"SOLIDWALL")));
        let proj = attrs("IFCPROJECT", 0).unwrap().unwrap();
        assert!(!proj[1].optional, "OwnerHistory is required in IFC2X3");
        let si = attrs("IFCSIUNIT", 1).unwrap().unwrap();
        assert!(si[0].derived, "Dimensions is derived in IfcSIUnit");
    }

    #[test]
    fn values_are_checked_against_the_declared_type() {
        let a = |s: &'static str| parse(s).unwrap().remove(0);
        assert!(check(&a("s"), &Value::Str("x".into())).is_ok());
        assert!(check(&a("s"), &Value::Null).is_err());
        assert!(check(&a("?s"), &Value::Null).is_ok());
        assert!(check(&a("s"), &Value::Ref(3)).is_err());
        assert!(check(&a("r"), &Value::Str("x".into())).is_err());
        assert!(check(&a("r"), &Value::Ref(3)).is_ok());
        assert!(check(&a("n"), &Value::Real(1.0)).is_ok());
        assert!(check(&a("n"), &Value::Int(1)).is_err());
        assert!(check(&a("u"), &Value::Int(1)).is_ok());
        assert!(check(&a("e=A|B"), &Value::Enum("A".into())).is_ok());
        assert!(check(&a("e=A|B"), &Value::Enum("C".into())).is_err());
        assert!(check(&a("e=A|B"), &Value::Str("A".into())).is_err());
        assert!(check(&a("l(r)"), &Value::List(vec![Value::Ref(1), Value::Ref(2)])).is_ok());
        assert!(check(&a("l(r)"), &Value::List(vec![Value::Ref(1), Value::Int(2)])).is_err());
        assert!(check(&a("l(r)"), &Value::Ref(1)).is_err());
        assert!(check(&a("*"), &Value::Derived).is_ok());
        assert!(check(&a("*"), &Value::Null).is_err());
        assert!(check(&a("s"), &Value::Derived).is_err());
        assert!(check(&a("?x"), &Value::Typed("IFCLABEL".into(), vec![])).is_ok());
    }

    #[test]
    fn schema_of_header_and_user_choice() {
        let h = |s: &str| format!("ISO-10303-21;\nHEADER;\nFILE_SCHEMA (( '{s}' ));\nENDSEC;\n");
        assert_eq!(IfcSchema::of_header(&h("IFC2X3")), Some(IfcSchema::Ifc2x3));
        assert_eq!(IfcSchema::of_header(&h("IFC4")), Some(IfcSchema::Ifc4));
        assert_eq!(IfcSchema::of_header(&h("IFC4X3_ADD2")), Some(IfcSchema::Ifc4x3));
        assert_eq!(IfcSchema::of_header(&h("ifc4x1")), Some(IfcSchema::Ifc4));
        assert_eq!(IfcSchema::of_header("HEADER;ENDSEC;"), None);
        assert_eq!(IfcSchema::of_header(&h("CIS2")), None);
        assert_eq!(IfcSchema::from_user("ifc2x3"), Some(IfcSchema::Ifc2x3));
        assert_eq!(IfcSchema::from_user(" IFC4X3 "), Some(IfcSchema::Ifc4x3));
        assert_eq!(IfcSchema::from_user("step"), None);
    }
}
