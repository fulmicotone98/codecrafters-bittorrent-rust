use std::env;

// Available if you need it!

#[allow(dead_code)]
fn decode_bencoded_value(encoded_value: &str) -> (serde_json::Value, &str) {
    match encoded_value.chars().next() {
        Some('0'..='9') => {
            // If encoded_value starts with a digit, it's a string
            // Example: "5:hello" -> "hello"
            if let Some((len, rest)) = encoded_value.split_once(':') {
                if let Ok(len) = len.parse::<usize>() {
                    (rest[..len].to_string().into(), &rest[len..])
                } else {
                    (serde_json::Value::Null, encoded_value)
                }
            } else {
                (serde_json::Value::Null, encoded_value)
            }
        }
        Some('i') => {
            // If encoded_value starts with 'i', it's an integer
            // Example: "i45e" -> 45
            if let Some((integer, rest)) =
                encoded_value
                    .split_at(1)
                    .1
                    .split_once('e')
                    .and_then(|(digits, rest)| {
                        let integer = digits.parse::<i64>().ok()?;
                        Some((integer, rest))
                    })
            {
                (integer.into(), rest)
            } else {
                (serde_json::Value::Null, encoded_value)
            }
        }
        Some('l') => {
            // If encoded_value starts with 'l', it's a list
            // For example: "l5:helloi52ee" -> ["hello", 52]

            // Value type represents any JSON type; since we will have a list of JSON vlaues, we can use a Vec of Value to store all the values of the bencoded list.
            let mut values = Vec::new();
            let mut rest = encoded_value.split_at(1).1;
            while !rest.is_empty() && !rest.starts_with('e') {
                let (val, remainder) = decode_bencoded_value(rest);
                values.push(val);
                rest = remainder;
            }

            (values.into(), &rest[1..])
        }
        Some('d') => {
            // If encoded_value starts with 'd', it's a dictionary.
            // For example: "d3:foo3:bar5:helloi52ee" -> {"hello": 52, "foo":"bar"}

            // Value type represents any JSON type; since we will have a list of JSON vlaues, we can use a Vec of Value to store all the values of the bencoded list.
            let mut dictionary = serde_json::Map::new();
            let mut rest = encoded_value.split_at(1).1;
            while !rest.is_empty() && !rest.starts_with('e') {
                let (k, remainder) = decode_bencoded_value(rest);
                let k = match k {
                    serde_json::Value::String(k) => k,
                    k => panic!("dictionary keys must be strings, not {k:?}"),
                }; 

                let (v, remainder) = decode_bencoded_value(remainder);
                dictionary.insert(k, v);

                rest = remainder;
            }

            (dictionary.into(), &rest[1..])
        }
        
        _ => (serde_json::Value::Null, encoded_value),
    }
}

// Usage: your_bittorrent.sh decode "<encoded_value>"
fn main() {
    let args: Vec<String> = env::args().collect();
    let command = &args[1];

    match command.as_str() {
        "decode" => {
            let encoded_value = &args[2];
            let decoded_value = decode_bencoded_value(encoded_value);
            println!("{}", decoded_value.0);
        }
        _ => println!("unknown command: {}", args[1]),
    }
}
