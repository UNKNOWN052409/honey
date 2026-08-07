/// 29 Android device models for Honeygain instance spoofing.
/// Each instance gets a unique model to avoid fingerprinting.
pub(crate) const ANDROID_MODELS: &[&str] = &[
    "Xiaomi 2311DRK48I Android 16",
    "Xiaomi 2306EPN60G Android 16",
    "Xiaomi 2107113SG Android 16",
    "Xiaomi Mi 14 Ultra Android 16",
    "Xiaomi Redmi Note 14 Pro Android 16",
    "Xiaomi Redmi K80 Pro Android 16",
    "Xiaomi Poco X7 Pro Android 16",
    "Samsung SM-S938B Android 16",
    "Samsung SM-S928B Android 16",
    "Samsung SM-F956B Android 16",
    "Samsung SM-A556B Android 16",
    "Samsung SM-A166B Android 16",
    "Samsung Galaxy S25 Ultra Android 16",
    "OnePlus CPH2581 Android 16",
    "OnePlus CPH2609 Android 16",
    "OnePlus 13 Android 16",
    "OnePlus 13R Android 16",
    "Oppo CPH2605 Android 16",
    "Oppo Find X8 Pro Android 16",
    "Vivo V2425 Android 16",
    "Vivo X200 Pro Android 16",
    "Realme RMX5000 Android 16",
    "Realme GT 8 Pro Android 16",
    "Honor Magic V4 Android 16",
    "Honor 400 Pro Android 16",
    "Google Pixel 10 Pro Android 16",
    "Nothing Phone 3a Android 16",
    "Motorola Moto G Power 2026 Android 16",
    "Asus Zenfone 12 Ultra Android 16",
];

/// 40 countries for ProxyRise sticky session IP diversity.
/// Instances are spread across these for maximum IP pool spread.
pub(crate) const SESSION_COUNTRIES: &[&str] = &[
    "us", "uk", "de", "jp", "ca", "au", "fr", "nl", "it", "es", "se", "no", "dk", "pl", "br", "in",
    "sg", "kr", "mx", "za", "tr", "ar", "ie", "ch", "at", "be", "pt", "gr", "cz", "ro", "hu", "il",
    "ae", "sa", "my", "th", "ph", "id", "vn", "nz",
];
