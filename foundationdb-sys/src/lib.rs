#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::unreadable_literal)]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

/// Defines the FDB_API_VERSION constant and generates the `if_cfg_api_versions` macro
/// from a list of (name, number) versions.
///
/// `if_cfg_api_versions` has one branch for all possible version combinations:
/// - (min = ...)
/// - (max = ...)
/// - (min = ..., max = ...) with min < max
///
/// For macro expansion reasons, we need to generate all the branches in a single expansion,
/// so this macro uses accumulators to gather all branch data before expanding it in one go.
/// In short, this is a combinatorial generator. Given `[0, 1, 2, 3]` it generates branches for:
/// - (min = 0)
/// - (min = 1)
/// - (min = 2)
/// - (min = 3)
/// - (max = 0)
/// - (max = 1)
/// - (max = 2)
/// - (max = 3)
/// - (min = 0, max = 1)
/// - (min = 0, max = 2)
/// - (min = 0, max = 3)
/// - (min = 1, max = 2)
/// - (min = 1, max = 3)
/// - (min = 2, max = 3)
macro_rules! versions_expand {
    ($(($version_name:tt, $version:tt)),+ $(,)?) => {
        // Entry point: start with a list of (version_name, version_number)
        // define the FDB_API_VERSION constant
        $(
            #[cfg(feature = $version_name)]
            pub const FDB_API_VERSION: u32 = $version;
        )*
        // Call the "@pre" phase with:
        // - visited_versions: []
        // - remaining_versions: versions
        // - acc2: [] (will store min branches)
        // - acc3: [] (will store max branches)
        versions_expand!(@pre [][$(($version_name, $version))+][][]);
    };
    (@pre [$($max:tt)*][$min:tt $($other:tt)*][$(($($acc2:tt)*))*][$(($($acc3:tt)*))*]) => {
        // "@pre" phase generates (min = ...) and (max = ...) branches
        // While remaining_versions is not empty:
        // - move one version from remaining to visited
        // - add one "min" branch to acc2
        // - add one "max" branch to acc3
        versions_expand!(@pre [$($max)* $min][$($other)*][$(($($acc2)*))* ($min $($other)*)][$(($($acc3)*))* ($min $($max)*)]);
    };
    (@pre [$min:tt $($version:tt)*][] $acc2:tt $acc3:tt) => {
        // remaining_versions is empty: end of "@pre" phase
        // Call the "@mid" phase with:
        // - min_range: [versions[0]]
        // - max_range: versions[1..]
        // - acc1: [] (will store min-max combination branches)
        // - acc2: min_branches
        // - acc3: max_branches
        versions_expand!(@mid [$min][$($version)*][] $acc2 $acc3);
    };
    (@mid [$min:tt $($version:tt)*][$max:tt $($other:tt)*][$(($($acc1:tt)*))*] $acc2:tt $acc3:tt) => {
        // "@mid" phase generates all (min = ..., max = ...) branches
        // This step generates all pairs for the current min
        // While the max_range is not empty:
        // - let max be the smallest version in max_range
        // - add the "min max" branch to acc1
        // - move max to the min_range
        versions_expand!(@mid [$min $($version)* $max][$($other)*][$(($($acc1)*))* ($min $max $($version)*)] $acc2 $acc3);
    };
    (@mid [$drop:tt $min:tt $($version:tt)*][] $acc1:tt $acc2:tt $acc3:tt) => {
        // max_range is empty: end of inner "@mid" loop
        // - drop current min
        // - set the second smallest version in min_range as new min
        // - set all remaining versions as the max_range
        versions_expand!(@mid [$min][$($version)*] $acc1 $acc2 $acc3);
    };
    (@mid [$drop:tt][] $acc1:tt $acc2:tt $acc3:tt) => {
        // min_range is empty: end of "@mid" phase
        // Call the "@end" phase with:
        // - the $ sign so it can use it as a symbol in the macro
        // - acc1: min_max_branches
        // - acc2: min_branches
        // - acc3: max_branches
        versions_expand!(@end ($) $acc1 $acc2 $acc3);
    };
    (@end ($d:tt)
        [$((($min_name1:tt, $min1:tt) ($max_name1:tt, $max1:tt) $(($version_name1:tt, $version1:tt))*))*]
        [$((($min_name2:tt, $min2:tt) $(($version_name2:tt, $version2:tt))*))*]
        [$((($max_name3:tt, $max3:tt) $(($version_name3:tt, $version3:tt))*))*]
    ) => {
        // "@end" phase generates the macro in one expansion
        // For each branch type, generates the appropriate cfg attributes

        /// Similar to `cfg_api_versions` proc-macro, but as a regular macro.
        /// Can be used on expressions and non-inlined modules, unlike proc-macros which are unstable in this position.
        ///
        /// Any feature after the "min" and "max" arguments will be joined in the "any" predicate.
        ///
        /// This macro has two "flavors" which behave slightly differently.
        ///
        /// ### "if else" form
        /// ```rs
        /// if_cfg_api_versions!(condition => { block1 } else { block2 });
        /// let result = if_cfg_api_versions!(condition => { block1 } else { block2 });
        /// ```
        /// Expects two blocks, behaves like an expression returning the result of the active block.
        /// Example:
        /// ```rs
        /// let result = if_cfg_api_versions!(max = 520 => {
        ///     println!("5.x");
        ///     true
        /// } else {
        ///     println!("6.0+");
        ///     false
        /// });
        /// // expands to:
        /// let result = {
        ///     #[cfg(any(feature = "fdb-5_1", feature = "fdb-5_2"))]
        ///     {
        ///         println!("5.x");
        ///         true
        ///     }
        ///     #[cfg(not(any(feature = "fdb-5_1", feature = "fdb-5_2")))]
        ///     {
        ///         println!("6.0+");
        ///         false
        ///     }
        /// };
        /// ```
        ///
        /// ### "if" form
        /// ```rs
        /// if_cfg_api_versions!(condition => statement);
        /// ```
        /// Expects a single statement, only puts a guard over it without any wrapping.
        /// Useful for non-inline modules.
        /// Example:
        /// ```rs
        /// if_cfg_api_versions!(min = 520, max = 600, feature = "foo" =>
        ///     pub mod bar;
        /// );
        /// // expands to:
        /// #[cfg(any(feature = "fdb-5_2", feature = "fdb-6_0", feature = "foo"))]
        /// pub mod bar;
        /// ```
        #[macro_export]
        macro_rules! if_cfg_api_versions {
            // malformed branch
            ($d($k:tt = $v:tt),+ => $a:stmt;$d($b:stmt);+ $d(;)?) => {
                MULTIPLE_STATEMENTS_IN_IF_FORM__PLEASE_USE_A_BLOCK_OR_DUPLICATE_THE_MACRO
            };
            // min-max branches
            $(
                (min=$min1, max=$max1 $d(,feature = $feature:literal)* => {$d($then:tt)*} else {$d($else:tt)*}) => {{
                    #[cfg(any(feature=$min_name1 $(,feature=$version_name1)* ,feature=$max_name1 $d(,feature=$feature)*))]
                    { $d($then)* }
                    #[cfg(not(any(feature=$min_name1 $(,feature=$version_name1)* ,feature=$max_name1 $d(,feature=$feature)*)))]
                    { $d($else)* }
                }};
                (min=$min1, max=$max1 $d(,feature = $feature:literal)* => $d($then:tt)*) => {
                    #[cfg(any(feature=$min_name1 $(,feature=$version_name1)* ,feature=$max_name1 $d(,feature=$feature)*))]
                    $d($then)*
                };
            )*
            // min branches
            $(
                (min=$min2 $d(,feature = $feature:literal)* => {$d($then:tt)*} else {$d($else:tt)*}) => {{
                    #[cfg(any(feature=$min_name2 $(,feature=$version_name2)* $d(,feature=$feature)*))]
                    { $d($then)* }
                    #[cfg(not(any(feature=$min_name2 $(,feature=$version_name2)* $d(,feature=$feature)*)))]
                    { $d($else)* }
                }};
                (min=$min2 $d(,feature = $feature:literal)* => $d($then:tt)*) => {
                    #[cfg(any(feature=$min_name2 $(,feature=$version_name2)* $d(,feature=$feature)*))]
                    $d($then)*
                };
            )*
            // max branches
            $(
                (max=$max3 $d(,feature = $feature:literal)* => {$d($then:tt)*} else {$d($else:tt)*}) => {{
                    #[cfg(any($(feature=$version_name3,)* feature=$max_name3 $d(,feature=$feature)*))]
                    { $d($then)* }
                    #[cfg(not(any($(feature=$version_name3,)* feature=$max_name3 $d(,feature=$feature)*)))]
                    { $d($else)* }
                }};
                (max=$max3 $d(,feature = $feature:literal)* => $d($then:tt)*) => {
                    #[cfg(any($(feature=$version_name3,)* feature=$max_name3 $d(,feature=$feature)*))]
                    $d($then)*
                };
            )*
        }
    };
}

versions_expand![
    ("fdb-5_1", 510),
    ("fdb-5_2", 520),
    ("fdb-6_0", 600),
    ("fdb-6_1", 610),
    ("fdb-6_2", 620),
    ("fdb-6_3", 630),
    ("fdb-7_0", 700),
    ("fdb-7_1", 710),
    ("fdb-7_3", 730),
];
