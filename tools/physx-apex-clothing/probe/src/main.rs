use pvd_protocol::{Decoder, Endian, Event, Name};
use serde_json::json;
use std::{collections::BTreeMap, fs::File, io::{BufWriter, Read, Write}, path::PathBuf};

fn type_name(strings: &BTreeMap<u32, String>, name: Name) -> String {
    format!("{}::{}", strings.get(&name.namespace).map_or("", String::as_str),
        strings.get(&name.name).map_or("", String::as_str))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.len() != 3 {
        return Err("usage: pvd-apex-event-probe capture.pvdstream new-output-directory".into());
    }
    let directory = PathBuf::from(&arguments[2]);
    std::fs::create_dir(&directory)?;
    let mut output = BufWriter::new(File::create(directory.join("events.jsonl"))?);
    let mut input = File::open(&arguments[1])?;
    let mut decoder = Decoder::default();
    let mut buffer = vec![0; 64 * 1024];
    let mut strings = BTreeMap::<u32, String>::new();
    let mut instances = BTreeMap::<u64, String>::new();
    let mut event_index = 0u64;
    let mut renderer_bytes = 0usize;
    let mut sections = BTreeMap::<(u64, String), (usize, usize)>::new();
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 { break; }
        let groups = decoder.feed(&buffer[..count])?;
        let endian = decoder.header().map_or(Endian::Little, |header| header.endian);
        for group in groups {
            for (ordinal, event) in group.events.into_iter().enumerate() {
                let detail = match event {
                    Event::StringHandle { handle, value } => {
                        strings.insert(handle, value);
                        None
                    }
                    Event::CreateInstance { class, id } => {
                        let class = type_name(&strings, class);
                        instances.insert(id, class.clone());
                        Some(json!({"kind":"create", "id":id, "class":class}))
                    }
                    Event::DestroyInstance { id } => {
                        let class = instances.remove(&id).unwrap_or_default();
                        Some(json!({"kind":"destroy", "id":id, "class":class}))
                    }
                    Event::BeginSection { id, name, timestamp } => {
                        let name = strings.get(&name).cloned().unwrap_or_default();
                        sections.entry((id, name.clone())).or_default().0 += 1;
                        Some(json!({"kind":"section_begin", "source":id, "name":name, "timestamp":timestamp,
                            "class":instances.get(&id)}))
                    }
                    Event::EndSection { id, name, timestamp } => {
                        let name = strings.get(&name).cloned().unwrap_or_default();
                        sections.entry((id, name.clone())).or_default().1 += 1;
                        Some(json!({"kind":"section_end", "source":id, "name":name, "timestamp":timestamp,
                            "class":instances.get(&id)}))
                    }
                    Event::SetPropertyValue { id, property, datatype, items, data } => {
                        let property = strings.get(&property).cloned().unwrap_or_default();
                        let class = instances.get(&id).cloned().unwrap_or_default();
                        let datatype = type_name(&strings, datatype);
                        if property == "events" && class.ends_with("::PvdUserRenderer") {
                            renderer_bytes = renderer_bytes.checked_add(data.len()).ok_or("renderer bytes overflow")?;
                            if renderer_bytes > 256 * 1024 * 1024 { return Err("renderer probe exceeds 256 MiB".into()); }
                            let filename = format!("renderer-{event_index}.bin");
                            std::fs::write(directory.join(&filename), data.as_ref())?;
                            Some(json!({"kind":"renderer_flush", "id":id, "class":class,
                                "bytes":data.len(), "items":items, "payload":filename}))
                        } else if class.starts_with("Apex::") && datatype.ends_with("::ObjectRef") && data.len() == 8 {
                            let raw: [u8; 8] = data.as_ref().try_into()?;
                            let target = match endian { Endian::Little => u64::from_le_bytes(raw), Endian::Big => u64::from_be_bytes(raw) };
                            Some(json!({"kind":"reference", "id":id, "class":class, "property":property, "target":target}))
                        } else {
                            Some(json!({"kind":"property", "id":id, "class":class,
                                "property":property, "datatype":datatype, "items":items, "bytes":data.len(),
                                "prefix_hex":data.iter().take(32).map(|byte|format!("{byte:02x}")).collect::<String>()}))
                        }
                    }
                    Event::BeginSetPropertyValue { id, property, datatype } => {
                        let property = strings.get(&property).cloned().unwrap_or_default();
                        if property == "events" && instances.get(&id).is_some_and(|name| name.ends_with("::PvdUserRenderer")) {
                            return Err("chunked renderer property requires explicit probe support".into());
                        }
                        let _ = datatype;
                        None
                    }
                    _ => None,
                };
                if let Some(mut row) = detail {
                    row["event"] = json!(event_index);
                    row["group_offset"] = json!(group.offset);
                    row["ordinal_in_group"] = json!(ordinal);
                    row["stream"] = json!(group.stream_id);
                    row["group_timestamp"] = json!(group.timestamp);
                    serde_json::to_writer(&mut output, &row)?;
                    output.write_all(b"\n")?;
                }
                event_index += 1;
            }
        }
    }
    decoder.finish()?;
    output.flush()?;
    let sections: Vec<_> = sections.into_iter().map(|((source, name), (begins, ends))|
        json!({"source":source,"name":name,"begins":begins,"ends":ends})).collect();
    let summary = json!({"events":event_index,"renderer_bytes":renderer_bytes,"sections":sections,
        "frame_assignment":"none; original transport order only", "offset_scope":"start of original event group"});
    std::fs::write(directory.join("summary.json"), serde_json::to_vec_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
