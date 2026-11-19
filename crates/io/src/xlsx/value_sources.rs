// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Optional `ValueSources` sheet: one row per parameter value whose source is recorded, with
//! its Source and Verified columns. Written only when `Style.sources` is on. Import never
//! reads it, so a book's round trip is the same with or without the sheet; an old copy of it
//! is never carried over as a user sheet (it would describe values as they were then).

use ddc_model::Model;
use rust_xlsxwriter::Workbook;

pub(crate) const SHEET: &str = "ValueSources";
const HEAD: [&str; 8] =
    ["Table", "Id", "Parameter", "Source", "Set at", "Request", "Prompt", "Verified"];

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// `2023-11-14 22:13 UTC`: the same text on every machine.
pub(crate) fn utc(unix: i64) -> String {
    let (days, sec) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", sec / 3600, sec % 3600 / 60)
}

/// The sheet, after every other sheet; nothing when the model records no source.
pub(crate) fn write(model: &Model, wb: &mut Workbook) -> Result<(), String> {
    let rows = model.value_srcs().map_err(err)?;
    let ws = wb.add_worksheet();
    ws.set_name(SHEET).map_err(err)?;
    for (c, h) in HEAD.iter().enumerate() {
        ws.write_string(0, c as u16, *h).map_err(err)?;
    }
    for (i, ((t, id, name), s)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        ws.write_string(r, 0, t).map_err(err)?;
        ws.write_number(r, 1, *id as f64).map_err(err)?;
        ws.write_string(r, 2, name).map_err(err)?;
        ws.write_string(r, 3, s.kind.as_str()).map_err(err)?;
        ws.write_string(r, 4, utc(s.at)).map_err(err)?;
        // text, never a formula: a prompt that starts with '=' stays words
        ws.write_string(r, 5, s.request.as_deref().unwrap_or("")).map_err(err)?;
        ws.write_string(r, 6, s.prompt.as_deref().unwrap_or("")).map_err(err)?;
        let v = s.verified_at.map(utc).unwrap_or_default();
        ws.write_string(r, 7, v).map_err(err)?;
    }
    Ok(())
}
