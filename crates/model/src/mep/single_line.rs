//! Single-line diagram of a distribution board: data, connection table, plain SVG.

use super::circuit::{CircuitKind, CircuitReport, PanelSchedule};
use crate::{Error, Model};

/// One outgoing line of the board.
#[derive(Clone, Debug, PartialEq)]
pub struct Feeder {
    pub number: u32,
    pub name: String,
    pub kind: CircuitKind,
    pub poles: u32,
    pub breaker: f64,
    pub section: f64,
    pub devices: usize,
    pub load: f64,
    pub length: f64,
    /// Ids of the failed cable-table rules.
    pub failed: Vec<&'static str>,
}

/// The board with its outgoing lines.
#[derive(Clone, Debug, PartialEq)]
pub struct SingleLine {
    pub panel: i64,
    pub load: f64,
    pub current: f64,
    pub feeders: Vec<Feeder>,
}

impl SingleLine {
    pub fn from_schedule(s: &PanelSchedule) -> Self {
        let f = |r: &CircuitReport| Feeder {
            number: r.number,
            name: r.name.clone(),
            kind: r.kind,
            poles: r.poles,
            breaker: r.breaker,
            section: r.cable,
            devices: r.devices,
            load: r.load,
            length: r.length,
            failed: r.rules.iter().filter(|x| !x.ok).map(|x| x.rule).collect(),
        };
        SingleLine {
            panel: s.panel,
            load: s.load,
            current: s.current,
            feeders: s.rows.iter().map(f).collect(),
        }
    }

    /// Connection table: panel, breaker, cable, load per line.
    pub fn connections(&self) -> Vec<[String; 6]> {
        self.feeders
            .iter()
            .map(|f| {
                [
                    format!("comp:{}", self.panel),
                    format!("QF{} {:.0}A/{}P", f.number, f.breaker, f.poles),
                    format!("{:.1} mm2", f.section),
                    f.name.clone(),
                    format!("{:.0} W", f.load),
                    f.failed.join(","),
                ]
            })
            .collect()
    }

    /// Plain SVG: a bus on top, one drop per line with breaker, cable and load labels.
    /// Lines with failed rules are drawn in red.
    pub fn to_svg(&self) -> String {
        let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let step = 90.0;
        let w = (self.feeders.len().max(1) as f64) * step + 40.0;
        let mut s = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"260\" \
             font-family=\"sans-serif\" font-size=\"10\">\n"
        );
        s.push_str(&format!(
            "<text x=\"10\" y=\"14\">comp:{} {:.0} W {:.1} A</text>\n",
            self.panel, self.load, self.current
        ));
        s.push_str(&format!(
            "<line x1=\"10\" y1=\"30\" x2=\"{}\" y2=\"30\" stroke=\"black\" stroke-width=\"3\"/>\n",
            w - 10.0
        ));
        for (i, f) in self.feeders.iter().enumerate() {
            let x = 40.0 + i as f64 * step;
            let col = if f.failed.is_empty() { "black" } else { "red" };
            s.push_str(&format!(
                "<line x1=\"{x}\" y1=\"30\" x2=\"{x}\" y2=\"200\" stroke=\"{col}\"/>\n\
                 <rect x=\"{}\" y=\"60\" width=\"16\" height=\"20\" fill=\"none\" stroke=\"{col}\"/>\n\
                 <text x=\"{}\" y=\"54\">QF{} {:.0}A</text>\n\
                 <text x=\"{}\" y=\"110\">{:.1} mm2</text>\n\
                 <text x=\"{}\" y=\"215\">{}</text>\n\
                 <text x=\"{}\" y=\"228\">{:.0} W</text>\n",
                x - 8.0,
                x - 20.0,
                f.number,
                f.breaker,
                x + 4.0,
                f.section,
                x - 20.0,
                esc(&f.name),
                x - 20.0,
                f.load
            ));
        }
        s.push_str("</svg>\n");
        s
    }
}

impl Model {
    pub fn single_line(&self, panel: i64) -> Result<SingleLine, Error> {
        Ok(SingleLine::from_schedule(&self.panel_schedule(panel)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mep::RuleFinding;

    fn sched() -> PanelSchedule {
        let row = |n: u32, bad: bool| CircuitReport {
            id: n as i64,
            number: n,
            name: format!("P<{n}>"),
            kind: CircuitKind::Power,
            poles: 1,
            breaker: 16.0,
            cable: 2.5,
            devices: 3,
            load: 1000.0,
            current: 4.3,
            length: 10.0,
            drop: 1.0,
            issues: vec![],
            rules: vec![RuleFinding { rule: "EL-DRP-01", ok: !bad, value: 5.0, limit: 4.0, note: None }],
            cos_phi: 0.9,
            load_type: "office".into(),
            conductor: crate::Conductor::Cu,
            x0_estimated: false,
            laying: "conduit_wall".into(),
            ambient_c: 30.0,
            grouped: 1,
        };
        PanelSchedule { panel: 7, rows: vec![row(1, false), row(2, true)], load: 2000.0, current: 8.7, phases: [8.7, 0.0, 0.0] }
    }

    #[test]
    fn diagram_data_table_and_svg() {
        let d = SingleLine::from_schedule(&sched());
        assert_eq!(d.feeders.len(), 2);
        assert!(d.feeders[0].failed.is_empty());
        assert_eq!(d.feeders[1].failed, vec!["EL-DRP-01"]);
        let t = d.connections();
        assert_eq!(t[1][1], "QF2 16A/1P");
        assert_eq!(t[1][5], "EL-DRP-01");
        let svg = d.to_svg();
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("stroke=\"red\"").count(), 2);
        assert!(svg.contains("P&lt;1&gt;"));
    }
}
