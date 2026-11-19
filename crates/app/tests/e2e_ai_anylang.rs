// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Any language in, the same pipeline out. Briefs in many scripts (right-to-left, CJK, Indic,
//! Thai, emoji, mixed) go through every path that reads the person's text without a panic; the
//! model is offered the same tools whatever the language; the same structured tool arguments
//! give the same project; reasons render in the UI language from their keys.

#[path = "e2e_ai_common/mod.rs"]
mod e2e_ai_common;
mod e2e_common;
#[path = "app2_fx/mod.rs"]
mod app2_fx;

use app2_fx::fx;
use ddc_ai::fake::{Script, ScriptedProvider};
use ddc_ai::{estimate_text_tokens, toolset, Mode};
use ddc_app::ai::ui::{budget, logic, md, steps};
use ddc_app::editor::Editor;
use ddc_gen::project::{build_project, parse_spec, why};
use ddc_ui::{numparse, Catalog, Lang};
use e2e_ai_common::*;
use e2e_common::count;
use serde_json::{json, Value as J};
use std::collections::BTreeSet;

/// The same request: a two-storey family house of about 160 m2 with a 14 m2 kitchen, three
/// bedrooms, two bathrooms and a gable roof.
const BRIEFS_BASE: &[(&str, &str)] = &[
    ("en", "Two-storey family house, about 160 m², kitchen 14 m², three bedrooms, two bathrooms, gable roof."),
    ("de", "Zweigeschossiges Einfamilienhaus, ca. 160 m², Küche 14,5 m², drei Schlafzimmer, zwei Bäder, Satteldach."),
    ("es", "Casa familiar de dos plantas, unos 160 m², cocina de 14,5 m², tres dormitorios, dos baños, cubierta a dos aguas."),
    ("fr", "Maison familiale à deux niveaux, environ 160 m², cuisine 14,5 m², trois chambres, deux salles de bain, toit à deux pans."),
    ("ar", "منزل عائلي من طابقين، حوالي ١٦٠ م²، مطبخ ١٤٫٥ م²، ثلاث غرف نوم، حمامان، سقف جملوني."),
    ("he", "בית משפחתי בן שתי קומות, כ-160 מ\"ר, מטבח 14 מ\"ר, שלושה חדרי שינה, שני חדרי רחצה, גג משופע."),
    ("fa", "خانه خانوادگی دو طبقه، حدود ۱۶۰ متر مربع، آشپزخانه ۱۴ متر، سه اتاق خواب، دو حمام، سقف شیروانی."),
    ("zh", "两层独栋住宅，约160平方米，厨房14平方米，三间卧室，两间浴室，双坡屋顶。"),
    ("ja", "２階建ての家族向け住宅、約１６０㎡、キッチン１４㎡、寝室３室、浴室２室、切妻屋根。"),
    ("ko", "2층 단독주택, 약 160㎡, 주방 14㎡, 침실 3개, 욕실 2개, 박공지붕."),
    ("hi", "दो मंज़िला पारिवारिक घर, लगभग १६० वर्ग मीटर, रसोई १४ वर्ग मीटर, तीन शयनकक्ष, दो स्नानघर, ढलानदार छत।"),
    ("th", "บ้านเดี่ยวสองชั้น พื้นที่ประมาณ ๑๖๐ ตร.ม. ห้องครัว ๑๔ ตร.ม. ห้องนอนสามห้อง ห้องน้ำสองห้อง หลังคาจั่ว"),
    ("el", "Διώροφη οικογενειακή κατοικία, περίπου 160 τ.μ., κουζίνα 14,5 τ.μ., τρία υπνοδωμάτια, δύο μπάνια, δίρριχτη στέγη."),
    ("tr", "İki katlı müstakil ev, yaklaşık 160 m², mutfak 14,5 m², üç yatak odası, iki banyo, beşik çatı. IŞIK İĞNE"),
    ("vi", "Nhà ở gia đình hai tầng, khoảng 160 m², bếp 14,5 m², ba phòng ngủ, hai phòng tắm, mái dốc hai mái."),
    ("bn", "দোতলা পারিবারিক বাড়ি, প্রায় ১৬০ বর্গমিটার, রান্নাঘর ১৪ বর্গমিটার, তিনটি শোবার ঘর, দুটি বাথরুম।"),
    ("emoji", "🏠🏠 2 floors 👨‍👩‍👧‍👦 ~160 m² 🍳 kitchen 14 m² 🛏️×3 🛁×2 ⛰️ roof 🇩🇪🇯🇵"),
];

/// All briefs: the table above plus the Russian and mixed-script ones kept in the data file.
fn briefs() -> Vec<(&'static str, String)> {
    BRIEFS_BASE
        .iter()
        .map(|(l, b)| (*l, b.to_string()))
        .chain([("ru", fx("any_ru")), ("mixed", fx("any_mixed"))])
        .collect()
}

const MIN_LANGS: usize = 15;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

/// The first `n` characters: a free-text label the person reads, in the person's language.
fn label(s: &str, n: usize) -> String {
    s.chars().take(n).collect::<String>().trim().to_string()
}

/// The canonical arguments a model sends for the brief, whatever its language: ids and numbers.
/// Only the labels (`name`) carry the person's words.
fn spec_for(brief: &str) -> J {
    let room = label(brief, 12);
    json!({"typology": "house", "floors": 2, "name": label(brief, 40),
           "rooms": [{"kind": "living", "area": 25}, {"kind": "kitchen", "area": 14, "name": room},
                     {"kind": "bedroom", "area": 14, "count": 3}, {"kind": "bath", "area": 5, "count": 2}]})
}

fn tool_names(p: &ScriptedProvider, k: usize) -> Vec<String> {
    p.requests()[k].tools.iter().map(|t| t.name.clone()).collect()
}

/// Every char-boundary prefix of `s`: the cut a stream or a limit can make anywhere.
fn prefixes(s: &str) -> impl Iterator<Item = &str> {
    s.char_indices().map(|(i, _)| &s[..i]).chain(std::iter::once(s))
}

#[test]
fn briefs_cover_the_scripts_asked_for() {
    let langs: BTreeSet<&str> = briefs().iter().map(|b| b.0).collect();
    assert!(langs.len() > MIN_LANGS, "{langs:?}");
    for l in ["ar", "he", "zh", "ja", "ko", "hi", "th", "el", "tr", "vi", "emoji", "mixed"] {
        assert!(langs.contains(l), "{l}");
    }
}

/// The text paths of the panel and the loop: token estimate, trim to fit, secret masking, the
/// palette, markdown, failure reasons, attachments, intent words and number fields.
#[test]
fn every_text_path_takes_any_script_cut_anywhere() {
    for (lang, brief) in briefs() {
        let brief = brief.as_str();
        let mut last = 0;
        for p in prefixes(brief) {
            let t = estimate_text_tokens(p);
            assert!(t >= last, "{lang}: the estimate never shrinks as text grows");
            last = t;
            let _ = logic::palette_offer(p, true);
            let _ = md::plain(&md::inline(p));
            let _ = md::md_blocks(p);
            let _ = steps::why(p);
            let _ = toolset::intent_groups(p);
            for w in p.split_whitespace() {
                let _ = numparse::parse(w, numparse::DecimalMark::Point);
                let _ = numparse::parse(w, numparse::DecimalMark::Comma);
            }
        }
        // dense scripts count at least a token a character, so a long brief is never underpriced
        let dense = brief.chars().filter(|c| c.len_utf8() >= 3).count() as u64;
        assert!(estimate_text_tokens(brief) >= dense, "{lang}");
        assert_eq!(logic::mask_secrets(brief), brief, "{lang}: a brief is not a secret");
        let long = brief.repeat(400);
        for context in [0u32, 200, 1000, 8000] {
            let cut = budget::trim_to_fit(&long, &[], context);
            assert!(long.starts_with(&cut), "{lang}: a cut on a character boundary");
            if context > 0 {
                assert_ne!(budget::measure(&cut, &[], context).level, budget::Level::Block, "{lang} {context}");
            }
        }
        let file = ddc_app::ai::attach::from_bytes("brief.txt", brief.as_bytes().to_vec(), 1).expect(lang);
        let mut req = ddc_app::ai::attach::Requirements::default();
        req.add_file(&file);
        req.confirmed = true;
        let block = ddc_app::ai::attach::context_block(&[std::sync::Arc::new(file)], &req);
        assert!(block.is_char_boundary(block.len()), "{lang}");
    }
}

/// Numbers typed the way each script writes them read as the same value.
#[test]
fn numbers_in_any_script_read_alike() {
    use numparse::DecimalMark::{Comma, Point};
    for (s, want) in [("14.5", 14.5), ("١٤٫٥", 14.5), ("۱۴٫۵", 14.5), ("१४.५", 14.5), ("๑๔.๕", 14.5), ("１４．５", 14.5)] {
        assert_eq!(numparse::parse(s, Point), Ok(want), "{s}");
        assert_eq!(numparse::parse(s, Comma), Ok(want), "{s}");
    }
    assert_eq!(numparse::parse("14,5", Comma), Ok(14.5));
    assert_eq!(numparse::parse("١٦٠", Point), Ok(160.0));
}

/// The whole loop with a scripted model: brief in, the same canonical `generate_project` call,
/// an answer in the person's language. Same tools offered, same project drafted and applied.
#[test]
fn the_same_tool_args_give_the_same_project_in_every_language() {
    let mut first: Option<(Vec<String>, Vec<String>, J, Vec<i64>)> = None;
    for (lang, brief) in briefs() {
        let brief = brief.as_str();
        let mut ed = editor();
        let answer = format!("**{}**\n\n- {brief}\n\n| {lang} | {brief} |\n|---|---|\n| 1 | 2 |", label(brief, 20));
        let provider = ScriptedProvider::new(vec![
            vec![tool("generate_project", json!({"spec": spec_for(brief)}))],
            vec![Script::Text(answer)],
        ]);
        let mut s = start(&mut ed, cfg(provider.clone()));
        turn(&mut s, &mut ed, brief, Mode::Propose);
        let results = tool_results(&provider);
        assert!(results.len() == 1 && !results[0].1, "{lang}: {results:?}");
        let reply = parsed(&results[0].0);
        let counts = reply["project"]["counts"].clone();
        assert!(counts.is_object(), "{lang}: {reply}");
        apply_and_check(&mut s, &mut ed);
        let model: Vec<i64> = ["level", "wall", "room", "opening", "slab", "stair", "roof"]
            .iter()
            .map(|t| count(&ed.model, t))
            .collect();
        let offered = (tool_names(&provider, 0), tool_names(&provider, 1));
        match &first {
            None => first = Some((offered.0, offered.1, counts, model)),
            Some((t0, t1, c, m)) => {
                assert_eq!(&offered.0, t0, "{lang}: the first request offers the same tools");
                assert_eq!(&offered.1, t1, "{lang}: and so does the next");
                assert_eq!(&counts, c, "{lang}: same args, same project");
                assert_eq!(&model, m, "{lang}: same model after Apply");
            }
        }
        // the system text is the same whatever the language of the request
        let sys0 = format!("{:?}", provider.requests()[0].messages[0].parts);
        assert!(sys0.contains("any language"), "{lang}");
    }
}

/// Reasons are keyed: every one renders in every UI language without leaking the key, English
/// reads exactly as the tool reply, Russian is translated with the same values.
#[test]
fn reasons_render_from_their_keys_in_the_ui_language() {
    let spec = parse_spec(&json!({"typology": "house", "floors": 2})).unwrap();
    let p = build_project(&spec).unwrap();
    let all: Vec<why::Why> = why::explain_project(&spec, &p).into_iter().chain(why::trade_offs(&spec, &p)).collect();
    assert!(all.len() >= 8, "{}", all.len());
    let en = Catalog::new(Lang::En);
    let ru = Catalog::new(Lang::Ru);
    for y in &all {
        assert!(ddc_gen::project::why_text::template(y.key).is_some(), "{} has no template", y.key);
        assert_eq!(ddc_app::gen_rules_ui::effect_text(&en, y), y.effect, "{}", y.key);
        let r = ddc_app::gen_rules_ui::effect_text(&ru, y);
        assert_ne!(r, y.effect, "{}: not translated", y.key);
        for (_, v) in &y.args {
            assert!(r.contains(v.as_str()), "{}: {r} lacks {v}", y.key);
        }
    }
    for &lang in Lang::ALL {
        let c = Catalog::new(lang);
        for y in &all {
            let t = ddc_app::gen_rules_ui::effect_text(&c, y);
            assert!(!t.contains("gen.why.e.") && !t.contains('{'), "{}: {t}", lang.code());
        }
    }
    // the English catalog says exactly what the tool reply says
    for (k, t) in ddc_gen::project::why_text::EN {
        assert_eq!(en.t(&format!("gen.why.e.{k}")), *t, "{k}");
    }
}

/// A spec's `lang` takes any UI locale as a BCP-47 tag; default room and level names come from
/// that locale's catalog (the UI's own fallback), not from an en/ru switch.
#[test]
fn spec_lang_takes_every_ui_locale_and_names_from_its_catalog() {
    use ddc_gen::project::names::{locale, locales};
    assert_eq!(locales().len(), Lang::ALL.len());
    for &l in Lang::ALL {
        assert!(locales().contains(&l.code()), "{}", l.code());
    }
    for (tag, want) in [("pt_BR", "pt-BR"), ("de-AT", "de"), ("JA", "ja"), ("iw", "he"), ("nb", "no"), ("xx", "en"), ("", "en")] {
        assert_eq!(locale(tag), want, "{tag}");
    }
    let en_cat = Catalog::new(Lang::En);
    let en = build_project(&parse_spec(&json!({"typology": "house", "floors": 2})).unwrap()).unwrap();
    let mut renamed = 0;
    for &l in Lang::ALL {
        let spec = parse_spec(&json!({"typology": "house", "floors": 2, "lang": l.code()})).expect(l.code());
        assert_eq!(spec.lang, l.code());
        let p = build_project(&spec).expect(l.code());
        let c = Catalog::new(l);
        for (k, st) in p.storeys.iter().enumerate() {
            let n = (k + 1).to_string();
            assert_eq!(st.name, c.tf("gen.name.level", &[("n", &n)]), "{}", l.code());
        }
        assert_eq!(p.roof.name, c.t("gen.name.roof"), "{}", l.code());
        // rooms the English project names by their kind's default take the locale's default;
        // programme rooms with their own en/ru names keep them
        for (st, st_en) in p.storeys.iter().zip(&en.storeys) {
            for ((g, r), g_en) in st.layout.rooms.iter().zip(&st.rooms).zip(&st_en.layout.rooms) {
                let key = format!("gen.name.room.{}", r.kind.key());
                // ru keeps its own programme tables
                if g_en.name == en_cat.t(&key) && l != Lang::Ru {
                    assert_eq!(g.name, c.t(&key), "{}", l.code());
                    renamed += 1;
                }
            }
        }
    }
    assert!(renamed > Lang::ALL.len(), "{renamed}");
    assert!(parse_spec(&json!({"typology": "house", "lang": 7})).is_err());
}

/// Room facts of an attached programme read numbers of any script; the label stays as written
/// and no kind is guessed from it.
#[test]
fn attached_room_areas_read_in_any_script() {
    use ddc_io::attach::facts::{fold_digits, rooms_of_table};
    let rows: Vec<Vec<String>> = [["Room", "Area", "Count"], ["مطبخ", "١٤٫٥", "١"], ["寝室", "１２", "３"], ["रसोई", "१४.५", "२"]]
        .iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .collect();
    let f = rooms_of_table("brief.xlsx", &rows);
    let got: Vec<(&str, Option<f64>, Option<u32>)> = f.iter().map(|x| (x.label.as_str(), x.area_m2, x.count)).collect();
    assert_eq!(got, vec![("مطبخ", Some(14.5), Some(1)), ("寝室", Some(12.0), Some(3)), ("रसोई", Some(14.5), Some(2))]);
    assert_eq!(fold_digits("٢٫٥ و ۳ और ४"), "2.5 و 3 और 4");
}

/// Fixes a conflict proposes carry a key: the window renders them in the UI language.
#[test]
fn conflict_fixes_render_in_the_ui_language() {
    let brief = json!({"typology": "office", "floors": 2, "rules": {"goal.efficiency": {"value": 0.98, "hard": true}}});
    let c = ddc_app::gen_rules_ui::conflict_of(&brief, None).expect("a conflict");
    assert!(!c.fixes.is_empty());
    let (en, ru) = (Catalog::new(Lang::En), Catalog::new(Lang::Ru));
    for (_, _, f) in &c.fixes {
        assert!(!f.key.is_empty(), "{f:?}");
        assert_eq!(ddc_app::gen_rules_ui::fix_text(&en, f), f.why);
        assert_ne!(ddc_app::gen_rules_ui::fix_text(&ru, f), f.why, "{}", f.key);
        for &l in Lang::ALL {
            assert!(!ddc_app::gen_rules_ui::fix_text(&Catalog::new(l), f).contains("gen.why.e."), "{}", l.code());
        }
    }
}
