use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteNode {
    pub name: String,
    pub province: String,
    pub city: String,
}

/// `String(x ?? '')` — null/missing become "", everything else stringifies. Never treats a
/// present empty string as "missing" (see the module doc for why that distinction matters).
fn field_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(_) => String::new(),
    }
}

/// `x ?? undefined` semantics as an `Option`: `None` only for null/missing — a present empty
/// string is `Some("")`, distinct from missing. Used for the `dc_name ?? hub_name ?? ...` chain.
fn field_nullish(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(other) => Some(field_string(Some(other))),
    }
}

/// JS truthiness for `if (raw.sgi_route_name)`: false for null/missing/""/0/false, true otherwise.
fn is_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// Mirrors `pick()`: first key whose value is present, non-null, and non-empty-string.
fn pick_field(raw: &Value, keys: &[&str]) -> String {
    for &k in keys {
        let s = field_string(raw.get(k));
        if !s.is_empty() {
            return s;
        }
    }
    String::new()
}

pub fn parse_route_detail_list(raw: &Value) -> Vec<RouteNode> {
    let Some(rdl) = raw.get("route_detail_list").and_then(Value::as_array) else {
        return Vec::new();
    };
    if rdl.is_empty() {
        return Vec::new();
    }
    let mut nodes = Vec::new();
    for entry in rdl {
        let Some(node_list) = entry.get("node_info_list").and_then(Value::as_array) else {
            continue;
        };
        for n in node_list {
            let name = field_string(n.get("name"));
            if name.is_empty() {
                continue;
            }
            let addr = n.get("address_info");
            let province = addr.map(|a| field_string(a.get("l1"))).unwrap_or_default();
            let city = addr.map(|a| field_string(a.get("l2"))).unwrap_or_default();
            nodes.push(RouteNode { name, province, city });
        }
    }
    nodes
}

pub fn parse_route_stops(raw: &Value) -> Vec<String> {
    // 1. Pre-enriched stored array (highest priority). Returns unconditionally once the RAW
    // array is non-empty, even if every entry filters out to nothing (matches TS: the length
    // check is on the raw array, not the post-filter result).
    if let Some(arr) = raw.get("route_stops").and_then(Value::as_array) {
        if !arr.is_empty() {
            return arr
                .iter()
                .map(|v| field_string(Some(v)))
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    // 2. route_detail_list (BEST source — actual DC names).
    let rdl_nodes = parse_route_detail_list(raw);
    if !rdl_nodes.is_empty() {
        return rdl_nodes.into_iter().map(|n| n.name).collect();
    }

    // 3. route_list / routes / route array — `dc_name ?? hub_name ?? name ?? location ?? ''`
    // per entry (nullish-coalescing chain, NOT "first non-empty").
    let route_list = raw
        .get("route_list")
        .and_then(Value::as_array)
        .or_else(|| raw.get("routes").and_then(Value::as_array))
        .or_else(|| raw.get("route").and_then(Value::as_array));
    if let Some(list) = route_list {
        if !list.is_empty() {
            let stops: Vec<String> = list
                .iter()
                .map(|r| {
                    field_nullish(r.get("dc_name"))
                        .or_else(|| field_nullish(r.get("hub_name")))
                        .or_else(|| field_nullish(r.get("name")))
                        .or_else(|| field_nullish(r.get("location")))
                        .unwrap_or_default()
                })
                .filter(|s| !s.is_empty())
                .collect();
            if !stops.is_empty() {
                return stops;
            }
        }
    }

    // 4. SGI enriched route string (truthiness check, not just non-null).
    if is_truthy(raw.get("sgi_route_name")) {
        let s = field_string(raw.get("sgi_route_name"));
        return s.split(" -> ").filter(|p| !p.is_empty()).map(String::from).collect();
    }

    // 5. report_station_name (origin DC from bidding/list).
    let report_station = field_string(raw.get("report_station_name"));
    if !report_station.is_empty() {
        return vec![report_station];
    }

    // 6. Origin + destination DC names (`pick` semantics: skip null/undefined/empty-string).
    let o = pick_field(raw, &["origin_dc_name", "origin_hub", "from_dc_name", "origin_name"]);
    let d = pick_field(raw, &["dest_dc_name", "dest_hub", "to_dc_name", "dest_name"]);
    [o, d].into_iter().filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rdl(names: &[&str]) -> serde_json::Value {
        let node_info_list: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| json!({ "name": n, "address_info": { "l1": format!("PROV_{i}"), "l2": format!("CITY_{i}") } }))
            .collect();
        json!({ "route_detail_list": [{ "node_info_list": node_info_list }] })
    }

    mod parse_route_detail_list_tests {
        use super::*;

        #[test]
        fn extracts_ordered_node_names_from_route_detail_list() {
            let nodes = parse_route_detail_list(&rdl(&["Padang DC", "Cileungsi DC"]));
            let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
            assert_eq!(names, vec!["Padang DC", "Cileungsi DC"]);
        }

        #[test]
        fn captures_province_l1_from_address_info() {
            let nodes = parse_route_detail_list(&rdl(&["A DC", "B DC"]));
            assert_eq!(nodes[0].province, "PROV_0");
        }

        #[test]
        fn missing_route_detail_list_is_empty() {
            assert_eq!(parse_route_detail_list(&json!({})), vec![]);
        }

        #[test]
        fn node_info_list_not_an_array_is_skipped_no_panic() {
            let raw = json!({ "route_detail_list": [{ "node_info_list": "oops" }] });
            assert_eq!(parse_route_detail_list(&raw), vec![]);
        }

        #[test]
        fn nodes_with_empty_name_are_dropped() {
            let raw = json!({ "route_detail_list": [{ "node_info_list": [{ "name": "" }, { "name": "Real DC" }] }] });
            let nodes = parse_route_detail_list(&raw);
            let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
            assert_eq!(names, vec!["Real DC"]);
        }
    }

    mod parse_route_stops_tests {
        use super::*;

        #[test]
        fn regression_parses_route_from_route_detail_list() {
            let stops = parse_route_stops(&rdl(&["Banjarmasin 2 DC", "Pontianak DC"]));
            assert_eq!(stops, vec!["Banjarmasin 2 DC", "Pontianak DC"]);
        }

        #[test]
        fn pre_enriched_route_stops_array_wins_highest_priority() {
            let raw = json!({ "route_stops": ["Aceh DC", "Cileungsi DC"], "route_detail_list": [] });
            assert_eq!(parse_route_stops(&raw), vec!["Aceh DC", "Cileungsi DC"]);
        }

        #[test]
        fn falls_back_to_report_station_name_when_no_route_data() {
            let raw = json!({ "report_station_name": "Medan DC" });
            assert_eq!(parse_route_stops(&raw), vec!["Medan DC"]);
        }

        #[test]
        fn falls_back_to_origin_and_dest_dc_names() {
            let raw = json!({ "origin_dc_name": "X DC", "dest_dc_name": "Y DC" });
            assert_eq!(parse_route_stops(&raw), vec!["X DC", "Y DC"]);
        }

        #[test]
        fn completely_empty_raw_is_empty() {
            assert_eq!(parse_route_stops(&json!({})), Vec::<String>::new());
        }

        #[test]
        fn empty_route_detail_list_nodes_falls_through_does_not_return_empty() {
            let raw = json!({ "route_detail_list": [{ "node_info_list": [] }], "report_station_name": "Solo DC" });
            assert_eq!(parse_route_stops(&raw), vec!["Solo DC"]);
        }

        #[test]
        fn three_stop_route_preserved_in_order() {
            let stops = parse_route_stops(&rdl(&["Yogyakarta DC", "Purbalingga DC", "Banyumas DC"]));
            assert_eq!(stops, vec!["Yogyakarta DC", "Purbalingga DC", "Banyumas DC"]);
        }
    }
}
