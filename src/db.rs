use super::*;

use postgres::Client;

use serde_json::{json, Value};

use postgres::fallible_iterator::FallibleIterator;
use postgres::types::ToSql;
use postgres::types::Type as PostgresType;
use postgres::{Row, Statement};

use std::collections::HashMap;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use uuid::Uuid;

fn parse_query_list<F>(q: &str, filter_gen: F) -> Result<String, CompassError>
where
    F: Fn(&str) -> Result<String, CompassError>,
{
    let mut filters: Vec<String> = Vec::new();
    let iter = q.split_inclusive('_');

    let mut curr_filter = String::new();

    for val in iter {
        if val == "and_" {
            let filter_string = curr_filter.strip_suffix('_').unwrap_or(&curr_filter);
            let filter = filter_gen(filter_string)?;
            curr_filter = String::new();
            filters.push(filter);
            filters.push("&&".to_string());
        } else if val == "or_" {
            let filter_string = curr_filter.strip_suffix('_').unwrap_or(&curr_filter);
            let filter = filter_gen(filter_string)?;
            curr_filter = String::new();
            filters.push(filter);
            filters.push("||".to_string());
        } else {
            curr_filter += val;
        };
    }

    if !curr_filter.is_empty() {
        let filter = filter_gen(&curr_filter)?;
        filters.push(filter);
    }

    Ok(format!("({})", filters.join(" ")))
}

pub fn generate_one_field(
    v: &str,
    field: (&String, FieldQuery),
    jsonb_filters: &mut Vec<String>,
    other_filters: &mut Vec<String>,
    other_bindings: &mut Vec<String>,
    bind_index: usize,
) -> Result<(), CompassError> {
    match field.1 {
        FieldQuery::Range {
            min: _,
            max: _,
            ref aliases,
        } => {
            // if something gets directly found as a 'Range' query, it means someone used season=18 instead of like, season_min=16. so it actually, counter-intuitively, is like a numeric tag!
            let filters = parse_query_list(v, |x| {
                if x == "exists" {
                    Ok(format!("(exists($.{}))", field.0))
                } else if x == "notexists" {
                    Ok(format!("(!exists($.{}))", field.0))
                } else if let Some(n) = aliases.get(&x.to_uppercase()) {
                    Ok(format!("($.{} == {})", field.0, n))
                } else {
                    Ok(format!(
                        "($.{} == {})",
                        field.0,
                        x.parse::<i64>().map_err(CompassError::InvalidNumberError)?
                    ))
                }
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::Min => {
            let filters = parse_query_list(v, |x| {
                Ok(format!(
                    "($.{} > {})",
                    field.0,
                    x.parse::<i64>().map_err(CompassError::InvalidNumberError)?
                ))
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::Max => {
            let filters = parse_query_list(v, |x| {
                Ok(format!(
                    "($.{} < {})",
                    field.0,
                    x.parse::<i64>().map_err(CompassError::InvalidNumberError)?
                ))
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::Bool => {
            let filters = parse_query_list(v, |x| {
                if x == "exists" {
                    Ok(format!("(exists($.{}))", field.0))
                } else if x == "notexists" {
                    Ok(format!("(!exists($.{}))", field.0))
                } else {
                    Ok(format!(
                        "($.{} == {})",
                        field.0,
                        x.parse::<bool>().map_err(CompassError::InvalidBoolError)?
                    ))
                }
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::AmbiguousTag => {
            let filters = parse_query_list(v, |x| {
                let mut filter: Vec<String> = Vec::new();

                if let Ok(n) = x.parse::<i64>() {
                    filter.push(format!("($.{} == {})", field.0, n)); // if it looks like an int, make it an int! because we can't specificy all the metadata fields in the schema. yeah i don't like this either
                } else if let Ok(n) = x.parse::<bool>() {
                    filter.push(format!("($.{} == {})", field.0, n));
                } else if x == "exists" {
                    filter.push(format!("(exists($.{}))", field.0))
                } else if x == "notexists" {
                    filter.push(format!("(!exists($.{}))", field.0))
                }

                filter.push(format!("($.{} == \"{}\")", field.0, x));

                Ok(format!("({})", filter.join(" || ")))
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::NumericTag { ref aliases } => {
            let filters = parse_query_list(v, |x| {
                if x == "exists" {
                    Ok(format!("(exists($.{}))", field.0))
                } else if x == "notexists" {
                    Ok(format!("(!exists($.{}))", field.0))
                } else if let Some(n) = aliases.get(&x.to_uppercase()) {
                    Ok(format!(
                        "(($.{field} == {value}) || ($.{field} == \"{value}\"))",
                        field = field.0,
                        value = n
                    ))
                } else {
                    Ok(format!(
                        "(($.{field} == {value}) || ($.{field} == \"{value}\"))",
                        field = field.0,
                        value = x.parse::<i64>().map_err(CompassError::InvalidNumberError)?
                    ))
                }
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::StringTag => {
            let filters = parse_query_list(v, |x| Ok(format!("($.{} == \"{}\")", field.0, x)))?;
            jsonb_filters.push(filters);
        }
        FieldQuery::Nested => {
            let filters = parse_query_list(v, |x| {
                let mut filter: Vec<String> = Vec::new();

                if let Ok(n) = x.parse::<i64>() {
                    filter.push(format!("($.{} == {})", field.0, n)); // if it looks like an int, make it an int! because we can't specificy all the metadata fields in the schema. yeah i don't like this either
                } else if let Ok(n) = x.parse::<bool>() {
                    filter.push(format!("($.{} == {})", field.0, n));
                } else if x == "exists" {
                    filter.push(format!("(exists($.{}))", field.0))
                } else if x == "notexists" {
                    filter.push(format!("(!exists($.{}))", field.0))
                }

                filter.push(format!("($.{} == \"{}\")", field.0, x));

                Ok(format!("({})", filter.join(" || ")))
            })?;
            jsonb_filters.push(filters);
        }
        FieldQuery::Fulltext {
            ref lang,
            ref syntax,
            ref target,
        } => {
            other_filters.push(format!(
                "to_tsvector('{lang}',object->>'{key}') @@ {function}('{lang}',${parameter})",
                lang = lang,
                key = target.as_ref().unwrap_or(field.0),
                function = syntax,
                parameter = other_filters.len() + bind_index
            ));
            other_bindings.push(v.to_string());
        }
        FieldQuery::Not(inner) => {
            // i hate myself
            let mut not_jsonb_filters = Vec::new();
            let mut not_other_bindings = Vec::new();
            let mut not_other_filters = Vec::new();
            generate_one_field(
                v,
                (field.0, *inner),
                &mut not_jsonb_filters,
                &mut not_other_bindings,
                &mut not_other_filters,
                bind_index,
            )?;

            jsonb_filters.extend(not_jsonb_filters.into_iter().map(|v| format!("!({})", v)));
        }
    };
    Ok(())
}

pub fn generate_where(
    schema: &Schema,
    fields: &HashMap<String, String>,
    bind_index: usize,
    force_json_query: bool,
) -> Result<(String, String, String, Vec<String>), CompassError> {
    let mut jsonb_filters = Vec::<String>::new();
    let mut other_filters = Vec::<String>::new();

    let mut other_bindings = Vec::<String>::new();

    for (k, v) in fields {
        let field_maybe = match schema.fields.get(k) {
            // find field from URL query in schema
            Some(field) => {
                Some((k.clone(), field.query.clone())) // oh, we found it by name. cool, return that
            }
            None => {
                let find_nested = |k: &str| {
                    schema.fields.iter().find_map(|f| {
                        match f.1.query {
                            // oops we couldn't find it; let's see if it's a field that can have multiple names like range or metadata
                            FieldQuery::Range {
                                ref min, ref max, ..
                            } => {
                                if k == min {
                                    Some((f.0.to_owned(), FieldQuery::Min))
                                } else if k == max {
                                    Some((f.0.to_owned(), FieldQuery::Max))
                                } else {
                                    None
                                }
                            }
                            FieldQuery::Nested => {
                                if k.split('.').next().unwrap() == f.0 {
                                    Some((k.to_owned(), FieldQuery::Nested))
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        }
                    })
                };

                if let Some(f) = k.strip_suffix('!') {
                    println!("{}", k);
                    // THE GOOD CODE DETECTED (JK IT'S VERY BAD THIS IS THE WORST THING I'VE EVER WRITTEN AND I'M DYING INSIDE)
                    schema
                        .fields
                        .get(f)
                        .map(|field| (k.clone(), FieldQuery::Not(Box::new(field.query.clone()))))
                        .or(find_nested(f).map(|(a, b)| (a, FieldQuery::Not(Box::new(b)))))
                } else {
                    find_nested(k)
                }
            }
        };

        if let Some(field) = field_maybe {
            generate_one_field(
                v,
                (&field.0, field.1),
                &mut jsonb_filters,
                &mut other_filters,
                &mut other_bindings,
                bind_index,
            )?;
        }
    }

    let json_query = format!("({})", jsonb_filters.join(" && "));

    // build out full query
    let query = if (!jsonb_filters.is_empty() || force_json_query) && other_filters.is_empty() {
        "WHERE object @@ CAST($1 AS JSONPATH)".to_owned()
    } else if (!jsonb_filters.is_empty() || force_json_query) && !other_filters.is_empty() {
        format!(
            "WHERE object @@ CAST($1 AS JSONPATH) AND {}",
            other_filters.join(" AND ")
        )
    } else if !other_filters.is_empty() {
        format!("WHERE {}", other_filters.join(" AND "))
    } else {
        String::new()
    };

    let order = match fields.get("sortorder") {
        Some(l) => {
            let ord = l.as_str().to_uppercase();
            if ord == "ASC" || ord == "DESC" {
                ord
            } else {
                "ASC".to_owned()
            }
        }
        None => "DESC".to_owned(),
    };

    let order_string = format!(
        " ORDER BY (object #> ($2)::text[]) {}, doc_id NULLS LAST LIMIT $3 OFFSET $4",
        order
    );

    Ok((query, order_string, json_query, other_bindings))
}

pub fn json_search(
    client: &mut Client,
    schema: &Schema,
    fields: &HashMap<String, String>,
    raw_query: Option<String>,
) -> Result<Vec<Value>, CompassError> {
    let converters: HashMap<String, ConverterSchema> = schema
        .fields
        .iter()
        .filter_map(|(k, v)| {
            v.converter.map(|converter| (k.to_owned(), converter))
        })
        .collect();

    let (query, sort_string, json_query, other_bindings) =
        generate_where(schema, fields, 5, raw_query.is_some())?;

    let json_query = if let Some(q) = raw_query {
        q
    } else {
        json_query
    };

    let query = format!(
        "SELECT object FROM {} {} {}",
        schema.table, query, sort_string
    );

    let statement: Statement = client
        .prepare_typed(query.as_str(), &[PostgresType::TEXT, PostgresType::TEXT])
        .map_err(CompassError::PGError)?;

    let sort_by = match fields.get("sortby") {
        Some(l) => l.as_str(),
        None => schema.default_order_by.as_str(),
    };

    let limit = match fields.get("limit") {
        Some(l) => l.parse::<i64>().map_err(CompassError::InvalidNumberError)?,
        None => 100,
    };

    let offset = match fields.get("offset") {
        Some(l) => l.parse::<i64>().map_err(CompassError::InvalidNumberError)?,
        None => 0,
    };

    let params: Vec<&dyn ToSql> = vec![&json_query, &sort_by, &limit, &offset];

    let rows: Vec<Row> = client
        .query_raw(
            &statement,
            params
                .iter()
                .copied()
                .chain(other_bindings.iter().map(|x| &*x as &dyn ToSql))
                .collect::<Vec<&dyn ToSql>>(),
        )
        .map_err(CompassError::PGError)?
        .collect()
        .map_err(CompassError::PGError)?;

    Ok(rows
        .into_iter()
        .map(|x| {
            let mut val = x.get::<usize, Value>(0);
            for (key, conv) in converters.iter() {
                if let Some(field) = val.get_mut(key) {
                    match (conv.from, conv.to) {
                        (ConvertFrom::DateTimeString, ConvertTo::Timestamp) => {
                            // convert timestamps back into date-strings
                            let timest = field.as_i64().unwrap();
                            let dt = DateTime::<Utc>::from_utc(
                                NaiveDateTime::from_timestamp(timest, 0),
                                Utc,
                            );
                            *field = json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                        }
                        (ConvertFrom::DateTimeString, ConvertTo::TimestampMillis) => {
                            let dt = Utc.timestamp_millis(field.as_i64().unwrap());
                            *field = json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                        }
                        _ => {}
                    }
                }
            }
            val
        })
        .collect())
}

pub fn json_count(
    client: &mut Client,
    schema: &Schema,
    fields: &HashMap<String, String>,
) -> Result<i64, CompassError> {
    let (query, _, json_query, other_bindings) = generate_where(schema, fields, 2, false)?;
    let query = format!("SELECT COUNT(*) FROM {} {}", schema.table, query);

    let statement: Statement = client
        .prepare_typed(query.as_str(), &[PostgresType::TEXT])
        .map_err(CompassError::PGError)?;

    let params: Vec<&dyn ToSql> = vec![&json_query];

    let res: Row = client
        .query_raw(
            &statement,
            params
                .iter()
                .copied()
                .chain(other_bindings.iter().map(|x| &*x as &dyn ToSql))
                .collect::<Vec<&dyn ToSql>>(),
        )
        .map_err(CompassError::PGError)?
        .next()?
        .unwrap();
    res.try_get::<usize, i64>(0).map_err(CompassError::PGError)
}

pub fn get_by_ids(
    client: &mut Client,
    schema: &Schema,
    ids: &Vec<Uuid>,
) -> Result<Vec<Value>, CompassError> {
    // make a table of field -> converter, to see if we need to do any conversions on the results
    let converters: HashMap<String, ConverterSchema> = schema
        .fields
        .iter()
        .filter_map(|(k, v)| {
            v.converter.map(|converter| (k.to_owned(), converter))
        })
        .collect();

    Ok(client
        .query(
            format!("SELECT object FROM {} WHERE doc_id = ANY($1)", schema.table).as_str(),
            &[ids],
        )?
        .into_iter()
        .map(|x| {
            let mut val = x.get::<usize, Value>(0);
            for (key, conv) in converters.iter() {
                if let Some(field) = val.get_mut(key) {
                    match (conv.from, conv.to) {
                        (ConvertFrom::DateTimeString, ConvertTo::Timestamp) => {
                            // convert timestamps back into date-strings
                            let timest = field.as_i64().unwrap();
                            let dt = DateTime::<Utc>::from_utc(
                                NaiveDateTime::from_timestamp(timest, 0),
                                Utc,
                            );
                            *field = json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                        }
                        (ConvertFrom::DateTimeString, ConvertTo::TimestampMillis) => {
                            let dt = Utc.timestamp_millis(field.as_i64().unwrap());
                            *field = json!(dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                        }
                        _ => {}
                    }
                }
            }
            val
        })
        .collect())
}
