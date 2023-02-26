use csv::ReaderBuilder;
use csv::WriterBuilder;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use google_maps::prelude::*;
use regex::Regex;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::default::Default;
use std::env;
use std::fs::File;
use std::io::prelude::*;

async fn get(url: String) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    let keys = vec![
        env::var("DEVELOPER_KEY0")?,
        env::var("DEVELOPER_KEY1")?,
        env::var("DEVELOPER_KEY2")?,
        env::var("DEVELOPER_KEY3")?,
        env::var("DEVELOPER_KEY4")?,
    ];
    static mut CURRENT_INDEX: usize = 0;
    let key: &str;
    unsafe {
        key = &keys[CURRENT_INDEX];
    }
    let url = url.to_owned() + "&key=" + key;
    unsafe {
        CURRENT_INDEX = (CURRENT_INDEX + 1) % keys.len();
    }
    match reqwest::get(url).await {
        Ok(response) => {
            if response.status().as_u16() == 403 {
                Err("exceeded youtube quota".into())
            } else {
                Ok(response)
            }
        }
        Err(err) => Err(Box::new(err) as Box<dyn std::error::Error>),
    }
}

#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
struct Id {
    videoId: String,
}

#[derive(Deserialize, Debug)]
struct Item {
    id: Id,
}

#[derive(Deserialize, Debug)]
struct SearchResult {
    items: Vec<Item>,
}

async fn search(query: &str) -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let mut xs = HashSet::new();
    let url = env::var("QUERY_URL_BASE")? + "&q=" + query;
    let response = get(url).await?;
    if let Ok(body) = response.json::<SearchResult>().await {
        for item in body.items {
            xs.insert(item.id.videoId);
        }
    }
    println!("search succeeded");
    Ok(xs)
}

async fn get_queries() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let url = env::var("QUERIES_URL")?;
    let gzip = reqwest::get(url).await?.bytes().await?;
    let mut d = GzDecoder::new(&*gzip);
    let mut s = String::new();
    d.read_to_string(&mut s).unwrap();
    let v: Vec<&str> = s.split('\n').collect();
    let mut ret: Vec<String> = vec![];
    for id in v {
        ret.push(id.to_string());
    }
    Ok(ret)
}

async fn get_blacklist() -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let url = env::var("BLACKLIST_URL")?;
    let gzip = reqwest::get(url).await?.bytes().await?;
    let mut d = GzDecoder::new(&*gzip);
    let mut s = String::new();
    d.read_to_string(&mut s).unwrap();
    let v: Vec<&str> = s.split('\n').collect();
    let mut ret = HashSet::<String>::new();
    for id in v {
        ret.insert(id.to_string());
    }
    Ok(ret)
}

async fn get_id_list() -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let xs = get_queries().await?;
    let mut ids = get_watchs().await?;
    let total = xs.len();
    for (count, query) in xs.into_iter().enumerate() {
        println!("search {count}/{total}");
        ids.extend(search(&query).await?);
    }
    Ok(ids)
}

async fn get_previous_id_list() -> Result<HashMap<String, (f64, f64)>, Box<dyn std::error::Error>> {
    let mut hm = HashMap::<String, (f64, f64)>::new();
    let url = env::var("DATA_URL")?;
    let gzip = reqwest::get(url).await?.bytes().await?;
    let mut d = GzDecoder::new(&*gzip);
    let mut s = String::new();
    d.read_to_string(&mut s).unwrap();
    let mut rdr = ReaderBuilder::new().from_reader(s.as_bytes());
    while let Some(result) = rdr.records().next() {
        let record = result?;
        let id = &record[2];
        let lat = record[0].parse()?;
        let lng = record[1].parse()?;
        hm.insert(id.to_string(), (lat, lng));
    }
    Ok(hm)
}

#[derive(Deserialize, Debug)]
struct Location {
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize, Debug)]
struct RecordingDetails {
    location: Location,
}

#[allow(non_snake_case)]
#[derive(Deserialize, Debug)]
struct VideoItem {
    recordingDetails: RecordingDetails,
}

#[derive(Deserialize, Debug)]
struct VideoResult {
    items: Vec<VideoItem>,
}

async fn get_location_from_youtube(id: &str) -> Result<(f64, f64), Box<dyn std::error::Error>> {
    let url = env::var("LOCATION_URL_BASE")? + "&id=" + id;
    let response = get(url).await?;
    match response.json::<VideoResult>().await {
        Ok(body) => {
            if !body.items.is_empty() {
                let location = &body.items[0].recordingDetails.location;
                Ok((location.latitude, location.longitude))
            } else {
                Ok((0.0, 0.0))
            }
        }
        Err(_) => Ok((0.0, 0.0)),
    }
}

async fn get_locations_from_youtube(
    ids: HashSet<&String>,
) -> Result<(HashMap<String, (f64, f64)>, HashSet<String>), Box<dyn std::error::Error>> {
    let mut locations = HashMap::<String, (f64, f64)>::new();
    let mut undefined = HashSet::<String>::new();
    let total = ids.len();
    for (count, &id) in ids.iter().enumerate() {
        println!("location_from_youtube {count}/{total}");
        let location = get_location_from_youtube(id).await?;
        if location != (0.0, 0.0) {
            println!("location_from_youtube found");
            locations.insert(id.to_string(), location);
        } else {
            println!("location_from_youtube not found");
            undefined.insert(id.to_string());
        }
    }
    Ok((locations, undefined))
}

fn remove_hashmap_keys_from_hashset<K, V>(hash_set: &mut HashSet<&K>, hash_map: &HashMap<K, V>)
where
    K: Eq + std::hash::Hash,
{
    hash_set.retain(|key| !hash_map.contains_key(key));
}

#[derive(Deserialize, Debug)]
struct VideoInfo {
    title: String,
    author_name: String,
}

async fn get_info(id: &str) -> Result<[String; 2], Box<dyn std::error::Error>> {
    let url = env::var("INFO_URL_BASE")? + "?v=" + id + "&format=json";
    let body = get(url).await?.json::<VideoInfo>().await?;
    Ok([body.title, body.author_name])
}

async fn get_location_from_map(id: &str, client: &ClientSettings) -> Result<(f64, f64), String> {
    match get_info(id).await {
        Ok(info) => {
            let address = info.join(" ");
            match client.geocoding().with_address(&address).execute().await {
                Ok(location) => match location.results.first() {
                    Some(result) => Ok((
                        result.geometry.location.lat.to_f64().unwrap(),
                        result.geometry.location.lng.to_f64().unwrap(),
                    )),
                    None => Err(address),
                },
                Err(err) => {
                    eprintln!("{err}");
                    Err(address)
                }
            }
        }
        Err(err) => {
            eprintln!("{err}");
            Err("".to_string())
        }
    }
}

async fn get_locations_from_map(
    ids: HashSet<String>,
) -> Result<(HashMap<String, (f64, f64)>, HashSet<String>), Box<dyn std::error::Error>> {
    let google_maps_client = ClientSettings::new(&env::var("GOOGLE_API_KEY")?);
    let total = ids.len();
    let mut blacklist = HashSet::<String>::new();
    let mut non_live_camera = HashSet::<String>::new();
    let mut locations = HashMap::<String, (f64, f64)>::new();
    for (count, id) in ids.into_iter().enumerate() {
        println!("location_from_map {count}/{total}");
        match get_location_from_map(&id, &google_maps_client).await {
            Ok(location) => {
                println!("location_from_map found");
                locations.insert(id.to_string(), location);
            }
            Err(info) => {
                println!("location_from_map not found");
                blacklist.insert(id.to_string());
                non_live_camera.insert(info);
            }
        }
    }
    write_hash_set(non_live_camera, "non_live_camera.txt.gz").await?;
    Ok((locations, blacklist))
}

#[derive(Debug, Serialize)]
struct Record<'a> {
    lat: f64,
    lng: f64,
    id: &'a str,
}

#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
struct Snippet {
    liveBroadcastContent: String,
}

#[allow(non_snake_case)]
#[derive(Deserialize, Debug)]
struct VideoItem2 {
    snippet: Snippet,
}

#[derive(Deserialize, Debug)]
struct VideoResult2 {
    items: Vec<VideoItem2>,
}

async fn is_live(id: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let url = env::var("LIVE_URL_BASE")? + "&id=" + id;
    let response = get(url).await?;
    match response.json::<VideoResult2>().await {
        Ok(body) => {
            if !body.items.is_empty() {
                Ok(&body.items[0].snippet.liveBroadcastContent == "live")
            } else {
                Ok(false)
            }
        }
        Err(_) => Ok(false),
    }
}

async fn remove_garbage(
    locations: &mut HashMap<String, (f64, f64)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut v: Vec<String> = vec![];
    for (count, (id, _)) in locations.iter().enumerate() {
        let locations_len = locations.len();
        println!("checking {count}/{locations_len}");
        if !is_live(id).await? {
            println!("invalid");
            v.push(id.to_string());
        }
    }
    for id in v {
        locations.remove(&id);
    }
    Ok(())
}

async fn write_geo(
    locations: HashMap<String, (f64, f64)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut wtr = WriterBuilder::new().has_headers(false).from_writer(vec![]);
    for (k, v) in locations {
        wtr.serialize(Record {
            lat: v.0,
            lng: v.1,
            id: &k,
        })?;
    }
    let data = wtr.into_inner()?;
    let file = File::create("geo.csv.gz")?;
    GzEncoder::new(file, Compression::default()).write_all(&data)?;
    Ok(())
}

async fn write_hash_set(
    blacklist: HashSet<String>,
    filename: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(filename)?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    for id in blacklist {
        match encoder.write_fmt(format_args!("{id}\n")) {
            Ok(_) => {}
            Err(_) => {
                println!("location not found");
                break;
            }
        }
    }
    Ok(())
}

#[derive(Deserialize, Debug)]
#[allow(non_snake_case)]
struct Request {
    totalResults: String,
    count: i32,
    startIndex: i32,
}

#[derive(Deserialize, Debug)]
struct Queries {
    request: Vec<Request>,
}

#[derive(Deserialize, Debug)]
struct SnippetItem {
    snippet: String,
}

#[derive(Deserialize, Debug)]
struct Watches {
    queries: Queries,
    items: Vec<SnippetItem>,
}

async fn get_watchs() -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let mut set = HashSet::<String>::new();
    let mut start = 1;
    loop {
        println!("get_watchs: start = {start}");
        let url = env::var("WATCH_URL")?.to_owned() + &start.to_string();
        let body = reqwest::get(url).await?.json::<Watches>().await?;
        let re = Regex::new(r"www\.youtube\.com/watch\?v=(.{11})").unwrap();
        for item in body.items {
            if let Some(caps) = re.captures(&item.snippet) {
                if let Some(s) = caps.get(1) {
                    set.insert(s.as_str().to_string());
                }
            }
        }
        let request = &body.queries.request[0];
        let total: i32 = request.totalResults.parse().unwrap();
        let next = request.startIndex + request.count;
        if next > total {
            break;
        }
        if next > 100 {
            break;
        } else {
            start = next;
        }
    }
    Ok(set)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args();
    let mut locations = get_previous_id_list().await?;
    let current_count = locations.len();
    remove_garbage(&mut locations).await?;
    if args.len() == 1 {
        let ids = get_id_list().await?;
        let mut blacklist = get_blacklist().await?;
        let mut ids = ids.difference(&blacklist).collect::<HashSet<&String>>();
        remove_hashmap_keys_from_hashset(&mut ids, &locations);
        let (locations_from_youtube, ids) = get_locations_from_youtube(ids).await?;
        locations.extend(locations_from_youtube);

        let (locations_from_map, ids) = get_locations_from_map(ids).await?;
        locations.extend(locations_from_map);
        blacklist.extend(ids);

        write_hash_set(blacklist, "blacklist.txt.gz").await?;
    }
    if locations.len() < current_count / 2 {
        let locations_len = locations.len();
        println!("new count of locations is too small: {locations_len} < {current_count} / 2");
        return Err("new count of locations is too small".into());
    }
    write_geo(locations).await?;
    Ok(())
}
