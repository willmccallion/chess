//! Search parameters that can be changed at runtime, for SPSA tuning.

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

macro_rules! search_params {
    ($($field:ident: $name:literal = $default:literal, $min:literal..=$max:literal, step $step:literal;)*) => {
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        pub struct SearchParams {
            $(pub $field: i32,)*
        }

        impl Default for SearchParams {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        pub const PARAM_SPECS: &[ParamSpec] = &[
            $(ParamSpec { name: $name, default: $default, min: $min, max: $max, step: $step },)*
        ];

        impl SearchParams {
            fn slot_mut(&mut self, name: &str) -> Option<&mut i32> {
                $(if name.eq_ignore_ascii_case($name) {
                    return Some(&mut self.$field);
                })*
                None
            }

            /// The value of the parameter called `name` (case-insensitive).
            pub fn get(&self, name: &str) -> Option<i32> {
                $(if name.eq_ignore_ascii_case($name) {
                    return Some(self.$field);
                })*
                None
            }
        }
    };
}

search_params! {
    rfp_margin: "RfpMargin" = 80, 20..=200, step 8;
    rfp_max_depth: "RfpMaxDepth" = 8, 4..=12, step 1;
    razor_margin: "RazorMargin" = 400, 100..=800, step 40;
    razor_depth_sq_margin: "RazorDepthSqMargin" = 250, 50..=500, step 25;
    nmp_base_reduction: "NmpBaseReduction" = 4, 2..=6, step 1;
    nmp_depth_divisor: "NmpDepthDivisor" = 3, 2..=6, step 1;
    nmp_eval_divisor: "NmpEvalDivisor" = 200, 80..=400, step 20;
    futility_base: "FutilityBase" = 100, 30..=250, step 10;
    futility_per_depth: "FutilityPerDepth" = 100, 30..=250, step 10;
    lmp_base: "LmpBase" = 3, 1..=8, step 1;
    see_quiet_margin: "SeeQuietMargin" = 50, 10..=150, step 5;
    see_capture_margin: "SeeCaptureMargin" = 90, 20..=200, step 8;
    history_pruning_margin: "HistoryPruningMargin" = 2048, 512..=6000, step 200;
    delta_margin: "DeltaMargin" = 200, 50..=500, step 20;
    probcut_margin: "ProbcutMargin" = 200, 80..=400, step 15;
    aspiration_delta: "AspirationDelta" = 25, 8..=60, step 3;
    singular_beta_per_depth_x16: "SingularBetaPerDepth" = 32, 8..=64, step 3;
    double_extension_margin: "DoubleExtensionMargin" = 20, 0..=60, step 3;
    lmr_quiet_base_x100: "LmrQuietBase" = 77, 0..=200, step 8;
    lmr_quiet_divisor_x100: "LmrQuietDivisor" = 236, 150..=400, step 15;
    lmr_capture_base_x100: "LmrCaptureBase" = 0, -100..=150, step 8;
    lmr_capture_divisor_x100: "LmrCaptureDivisor" = 300, 150..=500, step 20;
    lmr_history_divisor: "LmrHistoryDivisor" = 8192, 2048..=20000, step 600;
    deeper_margin: "DeeperMargin" = 70, 20..=150, step 6;
    shallower_margin: "ShallowerMargin" = 8, 0..=30, step 2;
}

impl ParamSpec {
    pub fn find(name: &str) -> Option<&'static ParamSpec> {
        PARAM_SPECS.iter().find(|spec| spec.name.eq_ignore_ascii_case(name))
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
        let Some(spec) = ParamSpec::find(name) else {
            return false;
        };
        match self.slot_mut(name) {
            Some(slot) => {
                *slot = spec.clamp(value);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_lie_inside_their_ranges() {
        for spec in PARAM_SPECS {
            assert!(spec.min <= spec.default && spec.default <= spec.max, "{}", spec.name);
            assert!(spec.step > 0, "{}", spec.name);
        }
    }

    #[test]
    fn set_clamps_to_the_range_and_get_reads_it_back() {
        let mut params = SearchParams::default();

        assert!(params.set("rfpmargin", 10_000));
        assert!(!params.set("NoSuchParam", 1));

        assert_eq!(params.get("RfpMargin"), Some(200));
        assert_eq!(params.rfp_margin, 200);
    }

    #[test]
    fn every_spec_names_a_field() {
        let params = SearchParams::default();
        for spec in PARAM_SPECS {
            assert_eq!(params.get(spec.name), Some(spec.default), "{}", spec.name);
        }
    }
}
