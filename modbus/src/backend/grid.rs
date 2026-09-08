//! Register presentation and derived-channel row construction.
use super::*;

pub(super) fn argb_to_color(argb: u32) -> slint::Color {
    slint::Color::from_argb_u8(
        (argb >> 24) as u8,
        (argb >> 16) as u8,
        (argb >> 8) as u8,
        argb as u8,
    )
}

/// Build the grid rows plus a parallel, index-aligned names vector. The names
/// vector is kept separate so the UI can drive the (editable) name column from a
/// stable model that is only refreshed when a name actually changes — otherwise
/// the per-poll value refresh would clobber in-progress typing.
pub(super) fn build_grid(
    rows: Vec<DisplayRow>,
    names: &HashMap<u16, String>,
    colors: &ColorRules,
    value_names: &ValueNames,
    editable: bool,
    area: Area,
) -> (Vec<crate::RegRow>, Vec<slint::SharedString>) {
    let mut regrows = Vec::with_capacity(rows.len());
    let mut namevec = Vec::with_capacity(rows.len());
    for r in rows {
        let nm: slint::SharedString = names
            .get(&(r.address as u16))
            .cloned()
            .unwrap_or_default()
            .into();
        let argb = r.num.map(|n| colors.eval(n)).unwrap_or(0);
        // #4: when a value-name annotation matches, show "value (label)".
        let value = match r.num.and_then(|n| value_names.lookup(n)) {
            Some(label) => format!("{} ({})", r.value, label),
            None => r.value,
        };
        regrows.push(crate::RegRow {
            address: r.address,
            name: nm.clone(),
            value: value.into(),
            raw: r.raw.into(),
            editable,
            colored: argb != 0,
            vcolor: argb_to_color(argb),
            hex_addr: format!("0x{:04X}", r.address).into(),
            plc_addr: area.plc_addr(r.address as u16).into(),
            auto_inc: false,
        });
        namevec.push(nm);
    }
    (regrows, namevec)
}

/// Polled values as u16 (bits map to 0/1) for derived-channel formulas.
pub(super) fn poll_values(data: &PollData) -> Vec<u16> {
    match data {
        PollData::Regs(v) => v.clone(),
        PollData::Bits(v) => v.iter().map(|&b| b as u16).collect(),
    }
}

pub(super) fn format_derived(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        // trim trailing zeros / dot
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Build the register grid then append derived-channel rows (evaluated over the
/// polled values). Derived rows carry a negative address so the UI shows "fx".
pub(super) fn master_grid(
    drows: Vec<DisplayRow>,
    vals: &[u16],
    names: &HashMap<u16, String>,
    colors: &ColorRules,
    value_names: &ValueNames,
    derived: &[DerivedCh],
    area: Area,
) -> (Vec<crate::RegRow>, Vec<slint::SharedString>) {
    let (mut rr, mut nn) = build_grid(drows, names, colors, value_names, false, area);
    for (di, ch) in derived.iter().enumerate() {
        let value = match crate::expr::eval_formula(&ch.formula, vals) {
            Ok(v) => format_derived(v),
            Err(_) => "—".to_string(),
        };
        let name: slint::SharedString = ch.name.clone().into();
        rr.push(crate::RegRow {
            address: -1 - di as i32,
            name: name.clone(),
            value: value.into(),
            raw: slint::SharedString::new(),
            editable: false,
            colored: false,
            vcolor: argb_to_color(0),
            hex_addr: "fx".into(),
            plc_addr: "fx".into(),
            auto_inc: false,
        });
        nn.push(name);
    }
    (rr, nn)
}
