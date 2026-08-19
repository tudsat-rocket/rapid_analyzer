//! Nitrous oxide saturation (vapour pressure) data, and the phase it implies
//! for a measured temperature/pressure pair.
//!
//! # Where the numbers come from
//!
//! [`SATURATION`] is the saturation table from the NIST Chemistry WebBook
//! (SRD 69), "Nitrous Oxide -- Saturation properties, temperature increments"
//! <https://webbook.nist.gov/cgi/fluid.cgi?ID=C10024972&Action=Page>, which
//! evaluates the Lemmon & Span (2006) short fundamental equation of state.
//! It was read off at 1 K steps from the triple point (182.33 K) to 309 K,
//! closed with the critical point, and is reproduced here verbatim -- no
//! curve fit of our own sits between the reference data and the plot. The
//! well-known ESDU 91022 correlation would have been a third of the lines,
//! but it is a fit to *other* people's data and disagrees with NIST by a few
//! tenths of a percent; for deciding whether a tank was saturated, that is
//! the same order as the thing being measured.
//!
//! Everything here is absolute pressure. A gauge reading has to have ambient
//! added to it before it means anything on this curve.

/// Triple point: below this there is no liquid, and the table stops.
pub const TRIPLE_T_K: f64 = 182.33;
pub const TRIPLE_P_KPA: f64 = 87.8373;

/// Critical point: above this there is no vapour/liquid boundary at all.
pub const CRITICAL_T_K: f64 = 309.52;
pub const CRITICAL_P_KPA: f64 = 7245.0;

/// Saturation pressure in kPa (absolute) against temperature in K, from the
/// triple point to the critical point. Strictly increasing in both columns,
/// which is what lets both lookups below binary-search it.
static SATURATION: [(f64, f64); 129] = [
    (182.330, 87.8373),
    (183.330, 93.3812),
    (184.330, 99.1986),
    (185.330, 105.299),
    (186.330, 111.692),
    (187.330, 118.387),
    (188.330, 125.393),
    (189.330, 132.722),
    (190.330, 140.382),
    (191.330, 148.384),
    (192.330, 156.739),
    (193.330, 165.456),
    (194.330, 174.547),
    (195.330, 184.021),
    (196.330, 193.891),
    (197.330, 204.166),
    (198.330, 214.858),
    (199.330, 225.978),
    (200.330, 237.537),
    (201.330, 249.547),
    (202.330, 262.020),
    (203.330, 274.965),
    (204.330, 288.397),
    (205.330, 302.325),
    (206.330, 316.763),
    (207.330, 331.722),
    (208.330, 347.213),
    (209.330, 363.251),
    (210.330, 379.846),
    (211.330, 397.011),
    (212.330, 414.758),
    (213.330, 433.100),
    (214.330, 452.050),
    (215.330, 471.620),
    (216.330, 491.822),
    (217.330, 512.671),
    (218.330, 534.178),
    (219.330, 556.356),
    (220.330, 579.219),
    (221.330, 602.780),
    (222.330, 627.051),
    (223.330, 652.047),
    (224.330, 677.780),
    (225.330, 704.264),
    (226.330, 731.511),
    (227.330, 759.537),
    (228.330, 788.353),
    (229.330, 817.975),
    (230.330, 848.414),
    (231.330, 879.686),
    (232.330, 911.804),
    (233.330, 944.782),
    (234.330, 978.633),
    (235.330, 1013.37),
    (236.330, 1049.01),
    (237.330, 1085.57),
    (238.330, 1123.06),
    (239.330, 1161.49),
    (240.330, 1200.87),
    (241.330, 1241.24),
    (242.330, 1282.59),
    (243.330, 1324.94),
    (244.330, 1368.30),
    (245.330, 1412.70),
    (246.330, 1458.14),
    (247.000, 1489.18),
    (248.000, 1536.40),
    (249.000, 1584.71),
    (250.000, 1634.12),
    (251.000, 1684.64),
    (252.000, 1736.29),
    (253.000, 1789.09),
    (254.000, 1843.04),
    (255.000, 1898.18),
    (256.000, 1954.50),
    (257.000, 2012.04),
    (258.000, 2070.79),
    (259.000, 2130.79),
    (260.000, 2192.04),
    (261.000, 2254.56),
    (262.000, 2318.37),
    (263.000, 2383.48),
    (264.000, 2449.92),
    (265.000, 2517.69),
    (266.000, 2586.81),
    (267.000, 2657.31),
    (268.000, 2729.20),
    (269.000, 2802.49),
    (270.000, 2877.21),
    (271.000, 2953.36),
    (272.000, 3030.98),
    (273.000, 3110.08),
    (274.000, 3190.68),
    (275.000, 3272.80),
    (276.000, 3356.45),
    (277.000, 3441.66),
    (278.000, 3528.44),
    (279.000, 3616.83),
    (280.000, 3706.84),
    (281.000, 3798.49),
    (282.000, 3891.80),
    (283.000, 3986.80),
    (284.000, 4083.51),
    (285.000, 4181.96),
    (286.000, 4282.17),
    (287.000, 4384.17),
    (288.000, 4487.98),
    (289.000, 4593.64),
    (290.000, 4701.17),
    (291.000, 4810.61),
    (292.000, 4921.98),
    (293.000, 5035.33),
    (294.000, 5150.68),
    (295.000, 5268.09),
    (296.000, 5387.58),
    (297.000, 5509.21),
    (298.000, 5633.03),
    (299.000, 5759.08),
    (300.000, 5887.43),
    (301.000, 6018.14),
    (302.000, 6151.29),
    (303.000, 6286.96),
    (304.000, 6425.26),
    (305.000, 6566.31),
    (306.000, 6710.27),
    (307.000, 6857.34),
    (308.000, 7007.84),
    (309.000, 7162.33),
    (309.520, 7245.00),
];

/// The table itself, for drawing the curve.
pub fn saturation_curve() -> &'static [(f64, f64)] {
    &SATURATION
}

/// Saturation pressure (kPa absolute) at `t_k`, or `None` outside the
/// liquid/vapour range where there is no such thing.
///
/// Interpolation is linear in `ln P`, which is what the Clausius-Clapeyron
/// relation says the curve nearly is; on a 1 K table that leaves an error far
/// below the reference data's own last digit.
pub fn psat_kpa(t_k: f64) -> Option<f64> {
    if !(TRIPLE_T_K..=CRITICAL_T_K).contains(&t_k) {
        return None;
    }
    let i = upper_index(&SATURATION, t_k, |row| row.0);
    let (t0, p0) = SATURATION[i - 1];
    let (t1, p1) = SATURATION[i];
    let f = (t_k - t0) / (t1 - t0);
    Some((p0.ln() + f * (p1.ln() - p0.ln())).exp())
}

/// Saturation (boiling) temperature in K at `p_kpa` absolute -- the inverse of
/// [`psat_kpa`], and `None` outside the same range.
pub fn tsat_k(p_kpa: f64) -> Option<f64> {
    if !(TRIPLE_P_KPA..=CRITICAL_P_KPA).contains(&p_kpa) {
        return None;
    }
    let i = upper_index(&SATURATION, p_kpa, |row| row.1);
    let (t0, p0) = SATURATION[i - 1];
    let (t1, p1) = SATURATION[i];
    let f = (p_kpa.ln() - p0.ln()) / (p1.ln() - p0.ln());
    Some(t0 + f * (t1 - t0))
}

/// Index of the first row whose `key` is at or above `value`, never 0 (so
/// `i - 1` is always a valid lower neighbour) and never past the end.
fn upper_index(rows: &[(f64, f64)], value: f64, key: fn(&(f64, f64)) -> f64) -> usize {
    rows.partition_point(|row| key(row) < value).clamp(1, rows.len() - 1)
}

/// Which side of the saturation curve a measurement sits on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Pressure above the vapour pressure: liquid, and not boiling.
    Liquid,
    /// On the curve within the tolerance band: liquid and vapour coexisting,
    /// which is where a self-pressurising tank spends its life.
    Saturated,
    /// Pressure below the vapour pressure: dry gas.
    Vapor,
    /// Above the critical temperature, where the distinction stops existing.
    Supercritical,
    /// Below the triple point: solid, and off the end of the data.
    BelowTriple,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Liquid => "liquid (subcooled)",
            Self::Saturated => "saturated (two-phase)",
            Self::Vapor => "vapour (superheated)",
            Self::Supercritical => "supercritical",
            Self::BelowTriple => "below the triple point",
        }
    }

    /// Short form for a dense readout.
    pub fn short(self) -> &'static str {
        match self {
            Self::Liquid => "liquid",
            Self::Saturated => "saturated",
            Self::Vapor => "vapour",
            Self::Supercritical => "supercritical",
            Self::BelowTriple => "solid",
        }
    }
}

/// Everything worth knowing about one (temperature, pressure) measurement,
/// in SI-ish base units: K and kPa absolute.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub t_k: f64,
    pub p_kpa: f64,
    pub phase: Phase,
    /// Vapour pressure at the measured temperature.
    pub psat_kpa: Option<f64>,
    /// Boiling temperature at the measured pressure.
    pub tsat_k: Option<f64>,
    /// `p - psat`: how far above (positive) or below the curve the point is.
    /// This is the quantity the state plot draws.
    pub margin_kpa: Option<f64>,
    /// `t - tsat`: the same distance measured along the temperature axis.
    pub superheat_k: Option<f64>,
}

impl State {
    /// How much colder / lower-pressure than critical this point is. Both are
    /// negative once past it.
    pub fn to_critical(&self) -> (f64, f64) {
        (CRITICAL_T_K - self.t_k, CRITICAL_P_KPA - self.p_kpa)
    }
}

/// Classifies a measurement.
///
/// `band` is the half-width of the "call it saturated" zone as a fraction of
/// the vapour pressure. It exists because a real tank at equilibrium never
/// reads exactly on the curve: a 1 % pressure transducer and a thermocouple a
/// kelvin out already put the point a bar off it, and calling that "subcooled
/// liquid" would be reading instrument error as physics.
pub fn state(t_k: f64, p_kpa: f64, band: f64) -> State {
    let psat_kpa = psat_kpa(t_k);
    let tsat_k = tsat_k(p_kpa);
    let margin_kpa = psat_kpa.map(|psat| p_kpa - psat);
    let superheat_k = tsat_k.map(|tsat| t_k - tsat);

    let phase = if t_k < TRIPLE_T_K {
        Phase::BelowTriple
    } else if t_k > CRITICAL_T_K {
        Phase::Supercritical
    } else {
        match (psat_kpa, margin_kpa) {
            (Some(psat), Some(margin)) if margin.abs() <= band * psat => Phase::Saturated,
            (_, Some(margin)) if margin > 0.0 => Phase::Liquid,
            (_, Some(_)) => Phase::Vapor,
            _ => Phase::Supercritical,
        }
    };

    State {
        t_k,
        p_kpa,
        phase,
        psat_kpa,
        tsat_k,
        margin_kpa,
        superheat_k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_strictly_increasing_in_both_columns() {
        for pair in SATURATION.windows(2) {
            assert!(pair[1].0 > pair[0].0, "temperature not increasing at {pair:?}");
            assert!(pair[1].1 > pair[0].1, "pressure not increasing at {pair:?}");
        }
        assert_eq!(SATURATION[0], (TRIPLE_T_K, TRIPLE_P_KPA));
        assert_eq!(SATURATION[SATURATION.len() - 1], (CRITICAL_T_K, CRITICAL_P_KPA));
    }

    /// Spot checks against the NIST WebBook rows the table was read from, and
    /// against the value every nitrous oxide data sheet quotes: about 50.5 bar
    /// absolute at 20 °C.
    #[test]
    fn the_curve_reproduces_its_reference_points() {
        assert!((psat_kpa(273.0).unwrap() - 3110.08).abs() < 0.01);
        assert!((psat_kpa(300.0).unwrap() - 5887.43).abs() < 0.01);
        assert!((psat_kpa(TRIPLE_T_K).unwrap() - TRIPLE_P_KPA).abs() < 0.01);
        assert!((psat_kpa(CRITICAL_T_K).unwrap() - CRITICAL_P_KPA).abs() < 0.01);

        let at_20c = psat_kpa(293.15).unwrap();
        assert!((at_20c - 5052.6).abs() < 1.0, "20 C vapour pressure was {at_20c} kPa");
    }

    #[test]
    fn interpolation_stays_between_the_rows_it_interpolates() {
        // Halfway between two table rows, the true curve is slightly below the
        // straight line between them, and ln-space interpolation has to land
        // in the gap rather than outside it.
        let mid = psat_kpa(293.5).unwrap();
        assert!(mid > 5035.33 && mid < 5150.68, "{mid}");
        let linear = (5035.33 + 5150.68) / 2.0;
        assert!(mid < linear, "convex curve: {mid} should be under the chord {linear}");
    }

    #[test]
    fn there_is_no_saturation_state_outside_the_two_phase_range() {
        assert_eq!(psat_kpa(310.0), None);
        assert_eq!(psat_kpa(150.0), None);
        assert_eq!(tsat_k(8000.0), None);
        assert_eq!(tsat_k(50.0), None);
    }

    #[test]
    fn pressure_and_temperature_lookups_are_inverses() {
        for t in [190.0, 230.0, 273.15, 293.15, 300.0, 309.0] {
            let p = psat_kpa(t).unwrap();
            let back = tsat_k(p).unwrap();
            assert!((back - t).abs() < 1e-6, "{t} K -> {p} kPa -> {back} K");
        }
    }

    #[test]
    fn a_tank_at_equilibrium_reads_as_saturated_despite_sensor_error() {
        let t = 293.15;
        let psat = psat_kpa(t).unwrap();
        // 1 % low, which any real transducer can be.
        let s = state(t, psat * 0.99, 0.02);
        assert_eq!(s.phase, Phase::Saturated);
        assert!(s.margin_kpa.unwrap() < 0.0);
        // Superheat is the same story told on the temperature axis: a hair
        // above the boiling point for that pressure.
        assert!(s.superheat_k.unwrap() > 0.0 && s.superheat_k.unwrap() < 0.5);
    }

    #[test]
    fn pressurising_above_the_curve_reads_as_liquid_and_venting_below_it_as_vapour() {
        let t = 293.15;
        let psat = psat_kpa(t).unwrap();
        assert_eq!(state(t, psat * 1.2, 0.02).phase, Phase::Liquid);
        assert_eq!(state(t, psat * 0.8, 0.02).phase, Phase::Vapor);
    }

    #[test]
    fn past_the_critical_temperature_there_is_no_phase_to_report() {
        let s = state(320.0, 8000.0, 0.02);
        assert_eq!(s.phase, Phase::Supercritical);
        assert_eq!(s.margin_kpa, None);
        assert!(s.to_critical().0 < 0.0);

        assert_eq!(state(170.0, 100.0, 0.02).phase, Phase::BelowTriple);
    }
}
