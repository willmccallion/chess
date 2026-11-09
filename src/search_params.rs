//! Search parameters that can be changed at runtime, for SPSA tuning.

use crate::search::{Depth, SearchScore};
use crate::types::Centipawns;

/// A tunable parameter: its UCI option name, default, range and SPSA perturbation size.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    /// Final SPSA perturbation `c_end`, in parameter units.
    pub step: i32,
}

/// A parameter's value, which tuning sees as a plain integer.
pub trait ParamValue: Copy {
    fn from_raw(raw: i32) -> Self;
    fn raw(self) -> i32;
}

impl ParamValue for i32 {
    fn from_raw(raw: i32) -> Self {
        raw
    }

    fn raw(self) -> i32 {
        self
    }
}

impl ParamValue for Depth {
    fn from_raw(raw: i32) -> Self {
        Self::new(raw)
    }

    fn raw(self) -> i32 {
        self.plies()
    }
}

impl ParamValue for SearchScore {
    fn from_raw(raw: i32) -> Self {
        Self::new(raw)
    }

    fn raw(self) -> i32 {
        self.units()
    }
}

impl ParamValue for Centipawns {
    fn from_raw(raw: i32) -> Self {
        Self::new(raw)
    }

    fn raw(self) -> i32 {
        self.get()
    }
}

macro_rules! search_params {
    ($($field:ident($name:literal): $ty:ty = $default:literal, $min:literal..=$max:literal, step $step:literal;)*) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        pub struct SearchParams {
            $(pub $field: $ty,)*
        }

        impl Default for SearchParams {
            fn default() -> Self {
                Self { $($field: ParamValue::from_raw($default),)* }
            }
        }

        pub const PARAM_SPECS: &[ParamSpec] = &[
            $(ParamSpec { name: $name, default: $default, min: $min, max: $max, step: $step },)*
        ];

        impl SearchParams {
            /// Sets the parameter called `name` (case-insensitive) to `raw`
            /// unclamped; false if there is none.
            fn set_raw(&mut self, name: &str, raw: i32) -> bool {
                $(if name.eq_ignore_ascii_case($name) {
                    self.$field = ParamValue::from_raw(raw);
                    return true;
                })*
                false
            }

            /// The value of the parameter called `name` (case-insensitive).
            #[cfg(test)]
            pub fn get(&self, name: &str) -> Option<i32> {
                $(if name.eq_ignore_ascii_case($name) {
                    return Some(self.$field.raw());
                })*
                None
            }
        }
    };
}

search_params! {
    rfp_margin("RfpMargin"): SearchScore = 77, 20..=200, step 8;
    rfp_max_depth("RfpMaxDepth"): Depth = 7, 4..=12, step 1;
    razor_margin("RazorMargin"): SearchScore = 399, 100..=800, step 40;
    razor_depth_sq_margin("RazorDepthSqMargin"): SearchScore = 247, 50..=500, step 25;
    nmp_base_reduction("NmpBaseReduction"): Depth = 4, 2..=6, step 1;
    nmp_depth_divisor("NmpDepthDivisor"): i32 = 2, 2..=6, step 1;
    nmp_eval_divisor("NmpEvalDivisor"): SearchScore = 204, 80..=400, step 20;
    futility_base("FutilityBase"): SearchScore = 97, 30..=250, step 10;
    futility_per_depth("FutilityPerDepth"): SearchScore = 99, 30..=250, step 10;
    lmp_base("LmpBase"): i32 = 5, 1..=8, step 1;
    see_quiet_margin("SeeQuietMargin"): Centipawns = 51, 10..=150, step 5;
    see_capture_margin("SeeCaptureMargin"): Centipawns = 90, 20..=200, step 8;
    history_pruning_margin("HistoryPruningMargin"): i32 = 2071, 512..=6000, step 200;
    delta_margin("DeltaMargin"): SearchScore = 208, 50..=500, step 20;
    probcut_margin("ProbcutMargin"): SearchScore = 193, 80..=400, step 15;
    aspiration_delta("AspirationDelta"): SearchScore = 22, 8..=60, step 3;
    singular_beta_per_depth_x16("SingularBetaPerDepth"): SearchScore = 33, 8..=64, step 3;
    double_extension_margin("DoubleExtensionMargin"): SearchScore = 20, 0..=60, step 3;
    lmr_quiet_base_x100("LmrQuietBase"): i32 = 79, 0..=200, step 8;
    lmr_quiet_divisor_x100("LmrQuietDivisor"): i32 = 239, 150..=400, step 15;
    lmr_capture_base_x100("LmrCaptureBase"): i32 = 1, -100..=150, step 8;
    lmr_capture_divisor_x100("LmrCaptureDivisor"): i32 = 302, 150..=500, step 20;
    lmr_history_divisor("LmrHistoryDivisor"): i32 = 8233, 2048..=20000, step 600;
    deeper_margin("DeeperMargin"): SearchScore = 69, 20..=150, step 6;
    shallower_margin("ShallowerMargin"): SearchScore = 9, 0..=30, step 2;
    nmp_min_depth("NmpMinDepth"): Depth = 2, 1..=6, step 1;
    nmp_eval_max("NmpEvalMax"): Depth = 3, 1..=6, step 1;
    iir_min_depth("IirMinDepth"): Depth = 3, 2..=8, step 1;
    move_pruning_max_depth("MovePruningMaxDepth"): Depth = 8, 4..=12, step 1;
    history_pruning_max_depth("HistoryPruningMaxDepth"): Depth = 4, 1..=8, step 1;
    probcut_min_depth("ProbcutMinDepth"): Depth = 5, 3..=8, step 1;
    probcut_reduction("ProbcutReduction"): Depth = 4, 2..=6, step 1;
    singular_min_depth("SingularMinDepth"): Depth = 8, 4..=12, step 1;
    singular_tt_depth_margin("SingularTtDepthMargin"): Depth = 4, 1..=6, step 1;
    lmr_min_depth("LmrMinDepth"): Depth = 2, 2..=5, step 1;
    lmr_non_pv_x1024("LmrNonPv"): i32 = 991, 0..=2048, step 64;
    lmr_cut_node_x1024("LmrCutNode"): i32 = 1017, 0..=2048, step 64;
    lmr_not_improving_x1024("LmrNotImproving"): i32 = 1020, 0..=2048, step 64;
    lmr_killer_or_counter_x1024("LmrKillerOrCounter"): i32 = 1008, 0..=2048, step 64;
    lmr_gives_check_x1024("LmrGivesCheck"): i32 = 1058, 0..=2048, step 64;
    history_bonus_quadratic("HistoryBonusQuadratic"): i32 = 15, 4..=40, step 2;
    history_bonus_linear("HistoryBonusLinear"): i32 = 31, 0..=96, step 4;
    history_bonus_constant("HistoryBonusConstant"): i32 = 15, -64..=96, step 6;
    history_bonus_max("HistoryBonusMax"): i32 = 1454, 500..=3000, step 100;
    correction_max_weight("CorrectionMaxWeight"): i32 = 16, 4..=48, step 2;
    lmr_tt_capture_x1024("LmrTtCapture"): i32 = 1025, 0..=2048, step 64;
    lmr_child_cutoffs_x1024("LmrChildCutoffs"): i32 = 993, 0..=2048, step 64;
    child_cutoff_threshold("ChildCutoffThreshold"): i32 = 4, 1..=8, step 1;
    lmr_tt_pv_x1024("LmrTtPv"): i32 = 1009, 0..=2048, step 64;
}

impl ParamSpec {
    pub fn find(name: &str) -> Option<&'static Self> {
        PARAM_SPECS
            .iter()
            .find(|spec| spec.name.eq_ignore_ascii_case(name))
    }

    pub const fn clamp(&self, value: i32) -> i32 {
        if value < self.min {
            self.min
        } else if value > self.max {
            self.max
        } else {
            value
        }
    }
}

impl SearchParams {
    /// Sets the parameter called `name`, clamped to its range; false if there is none.
    pub fn set(&mut self, name: &str, value: i32) -> bool {
        ParamSpec::find(name).is_some_and(|spec| self.set_raw(name, spec.clamp(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_lie_inside_their_ranges() {
        for spec in PARAM_SPECS {
            assert!(
                spec.min <= spec.default && spec.default <= spec.max,
                "{}",
                spec.name
            );
            assert!(spec.step > 0, "{}", spec.name);
        }
    }

    #[test]
    fn set_clamps_to_the_range_and_get_reads_it_back() {
        let mut params = SearchParams::default();

        assert!(params.set("rfpmargin", 10_000));
        assert!(!params.set("NoSuchParam", 1));

        assert_eq!(params.get("RfpMargin"), Some(200));
        assert_eq!(params.rfp_margin, SearchScore::new(200));
    }

    #[test]
    fn every_spec_names_a_field() {
        let params = SearchParams::default();
        for spec in PARAM_SPECS {
            assert_eq!(params.get(spec.name), Some(spec.default), "{}", spec.name);
        }
    }
}
