/// Every column in the accidents table.
/// Tuple: (canonical DB name, needs CAST(… AS VARCHAR) when selected).
/// All columns are quoted in SQL so names with special chars are handled safely.
pub const ALL_COLUMNS: &[(&str, bool)] = &[
    ("ID",                    false),
    ("Source",                false),
    ("Severity",              false),
    ("Start_Time",            true),
    ("End_Time",              true),
    ("Start_Lat",             false),
    ("Start_Lng",             false),
    ("End_Lat",               false),
    ("End_Lng",               false),
    ("Distance(mi)",          false),
    ("Description",           false),
    ("Street",                false),
    ("City",                  false),
    ("County",                false),
    ("State",                 false),
    ("Zipcode",               false),
    ("Country",               false),
    ("Timezone",              false),
    ("Airport_Code",          false),
    ("Weather_Timestamp",     true),
    ("Temperature(F)",        false),
    ("Wind_Chill(F)",         false),
    ("Humidity(%)",           false),
    ("Pressure(in)",          false),
    ("Visibility(mi)",        false),
    ("Wind_Direction",        false),
    ("Wind_Speed(mph)",       false),
    ("Precipitation(in)",     false),
    ("Weather_Condition",     false),
    ("Amenity",               false),
    ("Bump",                  false),
    ("Crossing",              false),
    ("Give_Way",              false),
    ("Junction",              false),
    ("No_Exit",               false),
    ("Railway",               false),
    ("Roundabout",            false),
    ("Station",               false),
    ("Stop",                  false),
    ("Traffic_Calming",       false),
    ("Traffic_Signal",        false),
    ("Turning_Loop",          false),
    ("Sunrise_Sunset",        false),
    ("Civil_Twilight",        false),
    ("Nautical_Twilight",     false),
    ("Astronomical_Twilight", false),
];

/// Case-insensitive lookup. Returns the canonical name + timestamp flag.
pub fn find_column(name: &str) -> Option<(&'static str, bool)> {
    ALL_COLUMNS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .copied()
}

/// Quoted column reference safe for WHERE / ORDER BY clauses.
pub fn col_ref(name: &str) -> String {
    format!(r#""{name}""#)
}
