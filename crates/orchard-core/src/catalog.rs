use crate::core::{now_ms, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub device_id: String,
    pub zip_code: String,
    #[serde(default)]
    pub part_number: String,
    #[serde(default = "unlocked")]
    pub carrier: String,
}
fn unlocked() -> String {
    "unlocked".into()
}
impl Default for Target {
    fn default() -> Self {
        Self {
            device_id: "iphone-duo".into(),
            zip_code: String::new(),
            part_number: String::new(),
            carrier: unlocked(),
        }
    }
}
impl Target {
    pub fn key(&self) -> String {
        let mut url = reqwest::Url::parse("apple://us/").unwrap();
        {
            let mut path = url.path_segments_mut().unwrap();
            path.push(&self.zip_code)
                .push(&self.device_id)
                .push(&self.carrier);
        }
        if !self.part_number.is_empty() {
            url.query_pairs_mut().append_pair("part", &self.part_number);
        }
        url.to_string()
    }
    pub fn from_key(key: &str) -> Result<Self> {
        let url = reqwest::Url::parse(key).map_err(|error| error.to_string())?;
        if url.scheme() == "apple" && url.host_str() == Some("us") {
            let p: Vec<_> = url.path().trim_matches('/').split('/').collect();
            if p.len() != 3 {
                return Err("Invalid Apple tracker identity.".into());
            }
            return Ok(Self {
                zip_code: p[0].into(),
                device_id: p[1].into(),
                carrier: p[2].into(),
                part_number: url
                    .query_pairs()
                    .find(|(key, _)| key == "part")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default(),
            });
        }
        // This decoder is migration-only. No requests are made to the old source.
        if url.scheme() == "https" && url.host_str() == Some("iphonechecker.herokuapp.com") {
            let p: Vec<_> = url.path().trim_matches('/').split('/').collect();
            if p.len() == 4 && p[0] == "q" {
                let id = match p[2] {
                    "iDuo" => "iphone-duo",
                    "i18Pro" => "iphone-18-pro",
                    "i18ProMax" => "iphone-18-pro-max",
                    _ => return Err("The old tracker uses an unsupported device.".into()),
                };
                return Ok(Self {
                    zip_code: p[1].into(),
                    device_id: id.into(),
                    carrier: p[3].into(),
                    part_number: String::new(),
                });
            }
        }
        Err("Select an Apple device and enter a US ZIP code.".into())
    }
    pub fn validate(&self, allow_empty_zip: bool) -> Result<()> {
        if !(allow_empty_zip && self.zip_code.is_empty())
            && (self.zip_code.len() != 5 || !self.zip_code.bytes().all(|c| c.is_ascii_digit()))
        {
            return Err("Enter a five-digit US ZIP code.".into());
        }
        if self.device_id.is_empty()
            || self.device_id.len() > 80
            || !self
                .device_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("Select a device from the Apple catalog.".into());
        }
        if !["unlocked", "att", "tmobile", "verizon"].contains(&self.carrier.as_str()) {
            return Err("Select a supported carrier.".into());
        }
        if !self.part_number.is_empty() && !valid_sku(&self.part_number) {
            return Err("Select a configuration from the catalog.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Variant {
    pub part_number: String,
    pub title: String,
    pub carrier: String,
    pub dimensions: BTreeMap<String, String>,
    pub standard_url: String,
    pub education_url: String,
    pub coming_soon: bool,
    #[serde(default)]
    pub details_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub category: String,
    pub buy_path: String,
    pub variants: Vec<Variant>,
    pub checked_at_ms: Option<i64>,
    pub last_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub schema_version: u32,
    pub devices: Vec<Device>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedTarget {
    pub target: Target,
    pub device_name: String,
    pub category: String,
    pub storefront_url: String,
    pub variants: Vec<Variant>,
}
impl Catalog {
    pub fn resolve(&self, target: &Target) -> Result<ResolvedTarget> {
        target.validate(false)?;
        let device = self
            .devices
            .iter()
            .find(|d| d.id == target.device_id)
            .ok_or("This device is not in the local catalog. Refresh it from Apple.")?;
        let variants: Vec<_> = device
            .variants
            .iter()
            .filter(|v| {
                (device.category != "iphone" || v.carrier == target.carrier)
                    && (target.part_number.is_empty() || v.part_number == target.part_number)
            })
            .cloned()
            .collect();
        if variants.is_empty() {
            return Err("The selected configuration is missing from the catalog. Refresh or choose another configuration.".into());
        }
        if device.category != "iphone" && target.carrier != "unlocked" {
            return Err("Carrier selection applies to iPhones only.".into());
        }
        let storefront_url = variants[0].standard_url.clone();
        Ok(ResolvedTarget {
            target: target.clone(),
            device_name: device.name.clone(),
            category: device.category.clone(),
            storefront_url,
            variants,
        })
    }
}

pub fn app_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("local.orchard.iphone-tracker")
}
pub fn valid_purchase_url(value: &str) -> bool {
    reqwest::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("www.apple.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && [
                "/shop/buy-iphone/",
                "/shop/buy-ipad/",
                "/shop/buy-mac/",
                "/shop/product/",
                "/us-edu/shop/buy-iphone/",
                "/us-edu/shop/buy-ipad/",
                "/us-edu/shop/buy-mac/",
                "/us-edu/shop/product/",
            ]
            .iter()
            .any(|prefix| url.path().starts_with(prefix))
    })
}
pub fn valid_sku(sku: &str) -> bool {
    sku.len() >= 8
        && sku.len() <= 30
        && sku.ends_with("LL/A")
        && sku.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'/')
}
pub fn builtin() -> Catalog {
    serde_json::from_str(include_str!("../data/catalog.json"))
        .expect("Bundled catalog must be valid")
}

pub struct CatalogStore {
    path: PathBuf,
}
impl CatalogStore {
    pub fn new(directory: &Path) -> Self {
        Self {
            path: directory.join("apple-catalog.json"),
        }
    }
    pub fn load(&self) -> Catalog {
        let mut catalog = builtin();
        if let Ok(text) = fs::read_to_string(&self.path) {
            if let Ok(cached) = serde_json::from_str::<Catalog>(&text) {
                if cached.schema_version == 1 {
                    for d in cached.devices {
                        if validate_page(&format!("https://www.apple.com{}", d.buy_path)).is_ok()
                            && d.variants.iter().all(|v| {
                                valid_sku(&v.part_number)
                                    && valid_purchase_url(&v.standard_url)
                                    && valid_purchase_url(&v.education_url)
                            })
                        {
                            if let Some(index) =
                                catalog.devices.iter().position(|old| old.id == d.id)
                            {
                                catalog.devices[index] = d;
                            } else {
                                catalog.devices.push(d);
                            }
                        }
                    }
                }
            }
        }
        catalog
    }
    pub fn save(&self, catalog: &Catalog) -> Result<()> {
        let parent = self.path.parent().ok_or("Catalog has no data directory.")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let temporary = parent.join(format!("apple-catalog-{}.tmp", now_ms()));
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(catalog).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&temporary, &self.path).map_err(|e| e.to_string())
    }
    pub fn refresh(&self, category: &str) -> Result<Catalog> {
        if !["iphone", "ipad", "mac"].contains(&category) {
            return Err("Choose iPhone, iPad, or Mac.".into());
        }
        let mut catalog = self.load();
        let paths: BTreeSet<_> = catalog
            .devices
            .iter()
            .filter(|d| d.category == category)
            .map(|d| d.buy_path.clone())
            .collect();
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let mut blocked = false;
        for path in paths {
            let result = if blocked || std::time::Instant::now() >= deadline {
                Err("Catalog refresh paused; the last good catalog is retained.".into())
            } else {
                fetch_page(&format!("https://www.apple.com{path}"))
                    .and_then(|html| parse_page(&html, &path))
            };
            match result {
                Ok(mut devices) => {
                    enrich_mac_devices(&mut devices, &catalog);
                    for device in devices {
                        if let Some(index) = catalog.devices.iter().position(|d| d.id == device.id)
                        {
                            catalog.devices[index] = device;
                        } else {
                            catalog.devices.push(device);
                        }
                    }
                }
                Err(error) => {
                    blocked |= error.contains("HTTP 541") || error.contains("HTTP 429");
                    for device in catalog.devices.iter_mut().filter(|d| d.buy_path == path) {
                        device.last_error = Some(error.clone());
                    }
                }
            }
        }
        self.save(&catalog)?;
        Ok(catalog)
    }
    pub fn add_page(&self, url: &str) -> Result<Catalog> {
        let path = validate_page(url)?;
        let mut catalog = self.load();
        let mut devices = parse_page(&fetch_page(url)?, &path)?;
        enrich_mac_devices(&mut devices, &catalog);
        for device in devices {
            if let Some(index) = catalog.devices.iter().position(|d| d.id == device.id) {
                catalog.devices[index] = device;
            } else {
                catalog.devices.push(device);
            }
        }
        self.save(&catalog)?;
        Ok(catalog)
    }
}

pub fn validate_page(url: &str) -> Result<String> {
    let url = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let p: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if url.scheme() != "https"
        || url.host_str() != Some("www.apple.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || p.len() != 3
        || p[0] != "shop"
        || !["buy-iphone", "buy-ipad", "buy-mac"].contains(&p[1])
        || !p[2].bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(
            "Use an Apple US buy page such as https://www.apple.com/shop/buy-ipad/ipad-pro.".into(),
        );
    }
    Ok(url.path().trim_end_matches('/').into())
}
fn fetch_page(url: &str) -> Result<String> {
    validate_page(url)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("OrchardInventoryTracker/0.2")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 3
                && attempt.url().scheme() == "https"
                && attempt.url().host_str() == Some("www.apple.com")
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Apple catalog request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Apple catalog returned HTTP {}. The last good catalog is retained.",
            response.status().as_u16()
        ));
    }
    use std::io::Read;
    let mut body = String::new();
    response
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() > 8 * 1024 * 1024 {
        return Err("Apple catalog page is too large.".into());
    }
    Ok(body)
}

fn plain(html: &str) -> String {
    let short = html
        .split("<as-footnote")
        .next()
        .unwrap_or(html)
        .split("<div")
        .next()
        .unwrap_or(html);
    scraper::Html::parse_fragment(short)
        .root_element()
        .text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn display(data: &Value, key: &str, value: &str) -> String {
    if key.to_ascii_lowercase().contains("capacity")
        && (value.ends_with("gb") || value.ends_with("tb"))
    {
        return value.to_uppercase();
    }
    let dict = data
        .get("displayValues")
        .or_else(|| data.get("mainDisplayValues"))
        .unwrap_or(&Value::Null);
    let item = &dict[key][value];
    let text = item
        .get("value")
        .or_else(|| item.get("header"))
        .and_then(Value::as_str)
        .map(plain)
        .unwrap_or_default();
    if !text.is_empty() {
        text
    } else if value.ends_with("gb") || value.ends_with("tb") {
        value.to_uppercase()
    } else if value.ends_with("inch") {
        format!("{}-inch", value.trim_end_matches("inch").replace('_', "."))
    } else if key.contains("CoreCount") {
        let values: Vec<_> = value.split('-').collect();
        let cores: Vec<_> = values
            .iter()
            .filter(|value| value.bytes().all(|b| b.is_ascii_digit()))
            .collect();
        if cores.len() == 2 {
            format!("{}-core CPU, {}-core GPU", cores[0], cores[1])
        } else {
            value.into()
        }
    } else {
        value.to_string()
    }
}
fn device_name(id: &str) -> String {
    match id {
        "iphone-17" => "iPhone 17",
        "iphone-17e" => "iPhone 17e",
        "iphone-air" => "iPhone Air",
        "iphone-duo" => "iPhone Duo",
        "iphone-18-pro" => "iPhone 18 Pro",
        "iphone-18-pro-max" => "iPhone 18 Pro Max",
        "ipad-pro" => "iPad Pro",
        "ipad-air" => "iPad Air",
        "ipad" => "iPad",
        "ipad-mini" => "iPad mini",
        "studio-display" => "Studio Display",
        "studio-display-xdr" => "Studio Display XDR",
        "macbook-pro-proled-xdr" => "MacBook Pro with ProLED XDR",
        "macbook-air" => "MacBook Air",
        "macbook-pro" => "MacBook Pro",
        "macbook-neo" => "MacBook Neo",
        "mac-mini" => "Mac mini",
        "imac" => "iMac",
        "mac-studio" => "Mac Studio",
        _ => {
            return id
                .split('-')
                .map(|part| format!("{}{}", part[..1].to_uppercase(), &part[1..]))
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
    .into()
}
pub fn parse_page(html: &str, buy_path: &str) -> Result<Vec<Device>> {
    let path = validate_page(&format!("https://www.apple.com{buy_path}"))?;
    let components: Vec<_> = path.split('/').collect();
    let category = components[2].trim_start_matches("buy-");
    let base_id = components[3];
    let doc = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("script").unwrap();
    let mut data = None;
    for script in doc.select(&selector) {
        let body = script.text().collect::<String>();
        if let Some(index) = body.find("productSelectionData") {
            let rest = body[index + "productSelectionData".len()..]
                .trim_start_matches('"')
                .trim_start();
            if let Some(rest) = rest.strip_prefix(':') {
                if let Some(Ok(value)) = serde_json::Deserializer::from_str(rest.trim_start())
                    .into_iter::<Value>()
                    .next()
                {
                    if value["products"].is_array() {
                        data = Some(value);
                        break;
                    }
                }
            }
        }
    }
    let data = data.ok_or(
        "Apple's product catalog structure was not recognized. The last good catalog is retained.",
    )?;
    let products = data["products"]
        .as_array()
        .ok_or("Apple catalog has no products.")?;
    let mut devices: BTreeMap<String, Device> = BTreeMap::new();
    for product in products {
        let sku = product
            .get("partNumber")
            .or_else(|| product.get("btrOrFdPartNumber"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !valid_sku(sku) {
            continue;
        }
        let carrier = match product.get("carrierModel").and_then(Value::as_str) {
            Some(c) if c.starts_with("ATT") => "att",
            Some(c) if c.starts_with("TMOBILE") => "tmobile",
            Some(c) if c.starts_with("VERIZON") => "verizon",
            _ => "unlocked",
        };
        // Wi-Fi and unlocked cellular iPads are represented once per sellable SKU.
        if category == "ipad" && carrier != "unlocked" {
            continue;
        }
        let family = product["familyType"].as_str().unwrap_or_default();
        let id = if base_id.ends_with("-pro") && family.contains("promax") {
            format!("{base_id}-max")
        } else {
            base_id.to_string()
        };
        let name = device_name(&id);
        let device = devices.entry(id.clone()).or_insert_with(|| Device {
            id: id.clone(),
            name: name.clone(),
            category: category.into(),
            buy_path: path.clone(),
            variants: Vec::new(),
            checked_at_ms: Some(now_ms()),
            last_error: None,
        });
        if device
            .variants
            .iter()
            .any(|v| v.part_number == sku && v.carrier == carrier)
        {
            continue;
        }
        let mut dimensions = BTreeMap::new();
        if let Some(object) = product.as_object() {
            for (key, value) in object
                .iter()
                .filter(|(key, _)| key.starts_with("dimension"))
            {
                if let Some(value) = value.as_str() {
                    dimensions.insert(
                        key.trim_start_matches("dimension").to_lowercase(),
                        display(&data, key, value),
                    );
                }
            }
        }
        if let Some(object) = product["dimensions"].as_object() {
            for (key, value) in object {
                if let Some(value) = value.as_str() {
                    let label = if key.contains("Screensize") {
                        "screensize"
                    } else if key.contains("Color") {
                        "color"
                    } else if key.contains("Capacity") {
                        "capacity"
                    } else if key.contains("Chip") && !key.contains("CoreCount") {
                        "chip"
                    } else if key.contains("CoreCount") {
                        "processor"
                    } else {
                        key
                    };
                    dimensions.insert(label.into(), display(&data, key, value));
                }
            }
        }
        if category == "mac" && !dimensions.contains_key("chip") {
            if let Some(container) = product["aosContainerPartNumber"].as_str() {
                if let Some(chip) = container.split('_').find(|part| {
                    part.starts_with('M') && part.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                }) {
                    dimensions.insert(
                        "chip".into(),
                        chip.replace("PRO", " Pro")
                            .replace("MAX", " Max")
                            .replace("ULTRA", " Ultra"),
                    );
                }
            }
        }
        let description = [
            "screensize",
            "capacity",
            "color",
            "connection",
            "finish",
            "chip",
            "processor",
        ]
        .iter()
        .filter_map(|key| dimensions.get(*key))
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ");
        let title = format!(
            "{name}{}{}",
            if description.is_empty() { "" } else { " · " },
            description
        );
        let token = product["seoUrlToken"].as_str().unwrap_or_default();
        let standard_url = if category == "iphone"
            && !token.is_empty()
            && token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
        {
            format!("https://www.apple.com{path}/{token}")
        } else {
            format!("https://www.apple.com/shop/product/{sku}")
        };
        let education_url = standard_url.replacen("/shop/", "/us-edu/shop/", 1);
        device.variants.push(Variant {
            part_number: sku.into(),
            title,
            carrier: carrier.into(),
            dimensions,
            standard_url,
            education_url,
            coming_soon: product["comingSoon"]
                .as_bool()
                .or_else(|| product["isComingSoon"].as_bool())
                .unwrap_or(false),
            details_error: None,
        });
    }
    let result: Vec<_> = devices
        .into_values()
        .map(|mut d| {
            d.variants.sort_by(|a, b| {
                a.title
                    .cmp(&b.title)
                    .then(a.part_number.cmp(&b.part_number))
            });
            d
        })
        .collect();
    if result.is_empty() {
        Err("Apple catalog contained no supported US pickup configurations. The last good catalog is retained.".into())
    } else {
        Ok(result)
    }
}

fn title_from_dimensions(name: &str, dimensions: &BTreeMap<String, String>) -> String {
    let fields = [
        "screensize",
        "chip",
        "processor",
        "memory",
        "capacity",
        "color",
        "connection",
        "finish",
    ];
    let values = fields
        .iter()
        .filter_map(|key| dimensions.get(*key))
        .cloned()
        .collect::<Vec<_>>();
    if values.is_empty() {
        name.into()
    } else {
        format!("{name} · {}", values.join(" · "))
    }
}
pub fn apply_mac_detail_url(variant: &mut Variant, name: &str, url: &str) -> Result<()> {
    if !valid_purchase_url(url) {
        return Err("Apple returned an unsupported product link.".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let tokens: Vec<_> = parsed
        .path()
        .split('/')
        .next_back()
        .unwrap_or_default()
        .split('-')
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        let unit = if token.ends_with("gb") {
            "GB"
        } else if token.ends_with("tb") {
            "TB"
        } else {
            continue;
        };
        let number = &token[..token.len() - 2];
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let after = &tokens[index + 1..tokens.len().min(index + 4)];
        if after.contains(&"memory") {
            variant
                .dimensions
                .insert("memory".into(), format!("{number}{unit} memory"));
        } else if after.contains(&"storage") {
            variant
                .dimensions
                .insert("capacity".into(), format!("{number}{unit} SSD"));
        }
    }
    variant.standard_url = url.into();
    variant.education_url = url.replacen("/shop/", "/us-edu/shop/", 1);
    variant.title = title_from_dimensions(name, &variant.dimensions);
    if !variant.dimensions.contains_key("memory") || !variant.dimensions.contains_key("capacity") {
        return Err(
            "Full Mac memory/storage details were not found in Apple's product link.".into(),
        );
    }
    variant.standard_url = url.into();
    variant.education_url = url.replacen("/shop/", "/us-edu/shop/", 1);
    variant.title = title_from_dimensions(name, &variant.dimensions);
    variant.details_error = None;
    Ok(())
}
pub fn enrich_mac_variant(variant: &mut Variant, name: &str) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("OrchardInventoryTracker/0.2")
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 4
                && attempt.url().scheme() == "https"
                && attempt.url().host_str() == Some("www.apple.com")
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .head(format!(
            "https://www.apple.com/shop/product/{}",
            variant.part_number
        ))
        .send()
        .map_err(|e| format!("Apple configuration details failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Apple configuration details returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    apply_mac_detail_url(variant, name, response.url().as_str())
}
fn enrich_mac_devices(devices: &mut [Device], previous: &Catalog) {
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut blocked = false;
    for device in devices
        .iter_mut()
        .filter(|d| d.category == "mac" && !d.id.starts_with("studio-display"))
    {
        for variant in &mut device.variants {
            if let Some(old) = previous.devices.iter().flat_map(|d| &d.variants).find(|v| {
                v.part_number == variant.part_number
                    && v.dimensions.contains_key("memory")
                    && v.dimensions.contains_key("capacity")
            }) {
                for key in ["memory", "capacity"] {
                    variant
                        .dimensions
                        .insert(key.into(), old.dimensions[key].clone());
                }
                variant.standard_url = old.standard_url.clone();
                variant.education_url = old.education_url.clone();
                variant.title = title_from_dimensions(&device.name, &variant.dimensions);
                continue;
            }
            let result = if blocked || std::time::Instant::now() >= deadline {
                Err("Configuration detail refresh paused; the base SKU remains available.".into())
            } else {
                enrich_mac_variant(variant, &device.name)
            };
            if let Err(error) = result {
                if error.contains("HTTP 541") || error.contains("HTTP 429") {
                    blocked = true;
                }
                variant.details_error = Some(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_ipad_and_mac_catalogs_produce_sellable_skus() {
        let ipad = parse_page(
            include_str!("../tests/fixtures/catalog-ipad-pro.html"),
            "/shop/buy-ipad/ipad-pro",
        )
        .unwrap();
        assert!(!ipad[0].variants.is_empty());
        assert!(ipad[0]
            .variants
            .iter()
            .all(|v| v.carrier == "unlocked" && valid_sku(&v.part_number)));
        let mac = parse_page(
            include_str!("../tests/fixtures/catalog-mac-mini.html"),
            "/shop/buy-mac/mac-mini",
        )
        .unwrap();
        assert_eq!(mac[0].variants.len(), 2);
        let mut variant = mac[0].variants[0].clone();
        apply_mac_detail_url(&mut variant,"Mac mini","https://www.apple.com/shop/buy-mac/mac-mini/m6-chip-12-core-cpu-12-core-gpu-16gb-memory-256gb-storage").unwrap();
        assert_eq!(variant.dimensions["memory"], "16GB memory");
        assert_eq!(variant.dimensions["capacity"], "256GB SSD");
        assert!(apply_mac_detail_url(&mut variant, "Mac mini", "https://example.com/a").is_err());
    }
    #[test]
    fn every_bundled_device_resolves_and_only_apple_purchase_links_are_used() {
        for device in builtin().devices {
            let target = Target {
                device_id: device.id.clone(),
                zip_code: "10001".into(),
                part_number: String::new(),
                carrier: "unlocked".into(),
            };
            let resolved = builtin().resolve(&target).unwrap();
            assert!(!resolved.variants.is_empty());
            for variant in resolved.variants {
                assert!(valid_sku(&variant.part_number));
                assert!(valid_purchase_url(&variant.standard_url));
                assert!(valid_purchase_url(&variant.education_url));
            }
        }
    }
    #[test]
    fn target_keys_are_native_and_legacy_targets_migrate() {
        let target =
            Target::from_key("https://iphonechecker.herokuapp.com/q/94025/iDuo/unlocked").unwrap();
        assert_eq!(target.key(), "apple://us/94025/iphone-duo/unlocked");
        assert_eq!(Target::from_key(&target.key()).unwrap(), target);
        let mut selected = target;
        selected.part_number = "MK1G4LL/A".into();
        assert_eq!(Target::from_key(&selected.key()).unwrap(), selected);
        assert!(validate_page("https://example.com/shop/buy-ipad/ipad-pro").is_err());
        assert!(
            validate_page("https://www.apple.com/shop/buy-ipad/ipad-pro?redirect=evil").is_err()
        );
    }
    #[test]
    fn parser_reads_data_without_executing_page_scripts() {
        let html = r#"<script>window.PRODUCT_SELECTION_BOOTSTRAP = {productSelectionData:{"products":[{"partNumber":"MK1G4LL/A","carrierModel":"UNLOCKED/US","dimensionCapacity":"512gb","dimensionColor":"starwhite","seoUrlToken":"7.6-inch-display-512gb-star-white-unlocked"}],"displayValues":{"dimensionColor":{"starwhite":{"value":"Star White"}}}}}; throw new Error('never run');</script>"#;
        let devices = parse_page(html, "/shop/buy-iphone/iphone-duo").unwrap();
        assert_eq!(devices[0].variants[0].part_number, "MK1G4LL/A");
        assert!(devices[0].variants[0].title.contains("512GB · Star White"));
        assert!(parse_page(
            "<script>productSelectionData: fetch('malicious')</script>",
            "/shop/buy-iphone/iphone-duo"
        )
        .is_err());
    }
}
