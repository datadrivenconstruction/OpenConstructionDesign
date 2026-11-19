//! Writers. Every write goes to a sibling temporary file and is renamed into place, and
//! nothing is ever written over an input file.

use std::path::{Path, PathBuf};

use ddc_io::ifc::{IfcOptions, IfcSchema};
use ddc_io::meshx::{MeshSet, Units};
use ddc_io::step::write::Schema;
use ddc_io::threemf::Unit;
use ddc_model::Model;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `None`: the schema of an IFC input, IFC4X3_ADD2 for any other input.
    Ifc(Option<IfcSchema>),
    Xlsx,
    Csv,
    /// Binary STL of the model-level bodies.
    Stl(Units),
    /// OBJ with an MTL next to it, one group per element.
    Obj(Units),
    /// Plans per level, two sections and the 3D faces.
    #[cfg(feature = "dxf-export")]
    Dxf(Units),
    /// Exact B-rep (STEP AP214 or AP242): typed solids with their faces, other elements as
    /// planar polyhedra; millimetres by default.
    Step(Schema, Units),
    /// 3MF core: one object per element, colours, a watertight check.
    ThreeMf(Unit),
    /// One SVG per sheet: the first at the output path, the others beside it.
    Svg,
}

impl Format {
    pub fn parse(s: &str) -> Result<Self, String> {
        let low = s.to_ascii_lowercase();
        let (base, unit_text) = match low.split_once(':') {
            Some((b, u)) => (b.to_string(), Some(u.to_string())),
            None => (low.clone(), None),
        };
        if base == "3mf" {
            let u = match &unit_text {
                None => Unit::default(),
                Some(t) => Unit::parse(t).ok_or_else(|| format!("unknown unit '{t}' in '{s}': expected um, mm, cm, in, ft or m"))?,
            };
            return Ok(Self::ThreeMf(u));
        }
        let unit = match &unit_text {
            None => None,
            Some(u) => Some(Units::parse(u).ok_or_else(|| format!("unknown unit '{u}' in '{s}': expected m or mm"))?),
        };
        let u = unit.unwrap_or_default();
        match base.as_str() {
            "step" | "stp" | "step214" => return Ok(Self::Step(Schema::Ap214, unit.unwrap_or(Units::Millimetre))),
            "step242" => return Ok(Self::Step(Schema::Ap242, unit.unwrap_or(Units::Millimetre))),
            "stl" => return Ok(Self::Stl(u)),
            "obj" => return Ok(Self::Obj(u)),
            #[cfg(feature = "dxf-export")]
            "dxf" => return Ok(Self::Dxf(u)),
            _ if unit.is_some() => return Err(format!("'{s}': only stl, obj, step, 3mf and dxf take a unit")),
            _ => {}
        }
        match low.as_str() {
            "ifc" => Ok(Self::Ifc(None)),
            "ifc2x3" => Ok(Self::Ifc(Some(IfcSchema::Ifc2x3))),
            "ifc4" => Ok(Self::Ifc(Some(IfcSchema::Ifc4))),
            "ifc4x3" => Ok(Self::Ifc(Some(IfcSchema::Ifc4x3))),
            "xlsx" => Ok(Self::Xlsx),
            "csv" => Ok(Self::Csv),
            "svg" => Ok(Self::Svg),
            o => Err(format!(
                "unknown export format '{o}': expected ifc, ifc2x3, ifc4, ifc4x3, xlsx, csv, stl, obj, step, step242, 3mf or svg{}",
                if cfg!(feature = "dxf-export") { " or dxf" } else { "" }
            )),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Ifc(_) => "ifc",
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Stl(_) => "stl",
            Self::Obj(_) => "obj",
            #[cfg(feature = "dxf-export")]
            Self::Dxf(_) => "dxf",
            Self::Step(Schema::Ap214, _) => "step",
            Self::Step(Schema::Ap242, _) => "step242",
            Self::ThreeMf(_) => "3mf",
            Self::Svg => "svg",
        }
    }
}

/// Path made absolute and comparable even when the file does not exist yet.
fn resolved(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let name = p.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::canonicalize(dir)
        .map(|d| d.join(&name))
        .unwrap_or_else(|_| p.to_path_buf())
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    let (x, y) = (resolved(a), resolved(b));
    if cfg!(windows) {
        x.to_string_lossy().to_lowercase() == y.to_string_lossy().to_lowercase()
    } else {
        x == y
    }
}

/// Refuses an output that is (or may be) one of the inputs. Returns the reason.
pub fn check_out(out: &Path, inputs: &[&Path]) -> Result<(), String> {
    match inputs.iter().find(|i| same_file(out, i)) {
        Some(i) => Err(format!("refusing to overwrite input file {}", i.display())),
        None => Ok(()),
    }
}

/// One row per element: `category,id,level,global_id`, the element's columns and properties.
pub fn csv_text(m: &Model) -> Result<String, String> {
    ddc_io::table_csv::csv_full(m)
}

/// Writes `fmt` of `m` to `out`; returns export warnings.
pub fn write(m: &Model, fmt: Format, out: &Path) -> Result<Vec<String>, String> {
    write_from(m, fmt, out, None)
}

/// [`write`] for a model read from `source`: an IFC output keeps the schema of an IFC source
/// unless the format names one.
pub fn write_from(
    m: &Model,
    fmt: Format,
    out: &Path,
    source: Option<&Path>,
) -> Result<Vec<String>, String> {
    let name = out
        .file_name()
        .ok_or("output has no file name")?
        .to_string_lossy();
    let tmp = out.with_file_name(format!(".{name}.ddc-tmp"));
    let res = (|| -> Result<Vec<String>, String> {
        match fmt {
            Format::Ifc(schema) => {
                let schema = schema.or_else(|| source.and_then(IfcSchema::of_file));
                let (text, warns) = match schema {
                    None => ddc_io::ifc::export_ifc_with_warnings(m)?,
                    Some(schema) => {
                        let o = IfcOptions { schema, ..Default::default() };
                        ddc_io::ifc::export_ifc_with(m, &o)?
                    }
                };
                std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
                Ok(warns)
            }
            Format::Xlsx => ddc_io::xlsx::export_xlsx(m, &tmp).map(|_| vec![]),
            Format::Csv => std::fs::write(&tmp, csv_text(m)?)
                .map(|_| vec![])
                .map_err(|e| e.to_string()),
            Format::Stl(u) => {
                let (set, warns) = MeshSet::from_model(m)?;
                ddc_io::stl::write_file(&set, u, &tmp)?;
                Ok(warns)
            }
            Format::Obj(u) => {
                let (set, warns) = MeshSet::from_model(m)?;
                let mtl = out.with_extension("mtl");
                let mtl_name = mtl.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let o = ddc_io::obj::ObjOptions { units: u, y_up: true };
                let (obj_text, mtl_text) = ddc_io::obj::write(&set, &mtl_name, o)?;
                std::fs::write(&tmp, obj_text).map_err(|e| e.to_string())?;
                let mtl_tmp = mtl.with_file_name(format!(".{mtl_name}.ddc-tmp"));
                std::fs::write(&mtl_tmp, mtl_text).map_err(|e| e.to_string())?;
                std::fs::rename(&mtl_tmp, &mtl).map_err(|e| {
                    let _ = std::fs::remove_file(&mtl_tmp);
                    format!("{}: {e}", mtl.display())
                })?;
                Ok(warns)
            }
            #[cfg(feature = "dxf-export")]
            Format::Dxf(u) => {
                let (set, mut warns) = MeshSet::from_model(m)?;
                let levels: Vec<(String, f64)> = m
                    .level_list()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|l| (l.1, l.2))
                    .collect();
                let o = ddc_io::mesh_dxf::DxfOptions::standard(&set, &levels, u);
                let st = ddc_io::mesh_dxf::write_file(&set, &o, &tmp)?;
                if st.lines == 0 {
                    warns.push("no plan or section line: the cuts miss every body".into());
                }
                Ok(warns)
            }
            Format::Step(schema, units) => {
                let (set, mut warns) = MeshSet::from_model(m)?;
                let w = ddc_io::step::write::WriteOptions {
                    schema,
                    units,
                    stamp: String::new(),
                };
                let title = out.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let o = ddc_io::exact::build::Opts::default();
                // all_solids: the model's solids and freeform patches join as exact parts
                let rep = ddc_io::exact::model::write_step(m, &set, true, &title, &w, &o, &tmp)?;
                warns.extend(rep.notes);
                Ok(warns)
            }
            Format::ThreeMf(unit) => {
                let (mut set, mut warns) = MeshSet::from_model(m)?;
                ddc_io::exact::model::add_solids(m, &mut set)?;
                let o = ddc_io::threemf::Options {
                    unit,
                    title: out.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    ..Default::default()
                };
                let rep = ddc_io::threemf::write_file(&set, &o, &tmp)?;
                for p in rep.leaky() {
                    warns.push(format!(
                        "{}: not watertight ({} open, {} bad edges)",
                        p.name, p.open_edges, p.bad_edges
                    ));
                }
                Ok(warns)
            }
            Format::Svg => svg_sheets(m, out, &tmp),
        }
    })();
    match res {
        Ok(w) => {
            std::fs::rename(&tmp, out).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                e.to_string()
            })?;
            Ok(w)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Every sheet as SVG: the first to `first` (the temporary output), the others beside `out`
/// as `<stem>-<number>.svg`.
fn svg_sheets(m: &Model, out: &Path, first: &Path) -> Result<Vec<String>, String> {
    use ddc_io::sheets::{file_name, print_composed, Ctx, LengthUnits, SheetRefs};
    let sheets = m.sheets().map_err(|e| e.to_string())?;
    if sheets.is_empty() {
        return Err("the model has no sheet".into());
    }
    let refs = SheetRefs::load(m).map_err(|e| e.to_string())?;
    let none = |_: &str| -> Option<ddc_io::sheets::Table> { None };
    // the sheet labels the editor's English catalog gives (no UI crate in the batch tool)
    let catalog: Vec<serde_json::Value> = [
        include_str!("../../ui/data/i18n/en/sheets_issue.json"),
        include_str!("../../ui/data/i18n/en/dset.json"),
    ]
    .iter()
    .filter_map(|t| serde_json::from_str(t).ok())
    .collect();
    let text = |k: &str| -> String {
        catalog
            .iter()
            .find_map(|c| c.get(k).and_then(|v| v.as_str()))
            .unwrap_or(k)
            .to_string()
    };
    let stem = out.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut warns = vec![];
    for (i, (id, def)) in sheets.iter().enumerate() {
        let ctx = Ctx {
            units: LengthUnits::Metric,
            schedules: &none,
            index: Some((i + 1, sheets.len())),
            paper: None,
            text: &text,
            refs: Some(&refs),
        };
        let c = print_composed(m, *id, &ctx).map_err(|e| format!("{}: {e}", def.number))?;
        warns.extend(c.warnings.iter().map(|w| format!("{}: {w}", def.number)));
        let o = ddc_io::svg::Options {
            title: format!("{} {}", def.number, def.name).trim().to_string(),
            ..Default::default()
        };
        let svg = ddc_io::svg::write(&c.drawing.to_pdf_sheet(), &o)?;
        if i == 0 {
            std::fs::write(first, svg).map_err(|e| e.to_string())?;
        } else {
            let p = out.with_file_name(file_name("{stem}-{number}", &def.number, &def.name, "svg").replace("{stem}", &stem));
            let tmp = p.with_file_name(format!(".{}.ddc-tmp", p.file_name().unwrap_or_default().to_string_lossy()));
            std::fs::write(&tmp, svg).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &p).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", p.display())
            })?;
        }
    }
    Ok(warns)
}
