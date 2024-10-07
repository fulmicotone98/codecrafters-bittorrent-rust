use std::env;

// Available if you need it!
// use serde_bencode

#[allow(dead_code)]
fn decode_bencoded_value(encoded_value: &str) -> serde_json::Value {
    
    // If encoded_value starts with a digit, it's a string
    if encoded_value.chars().next().unwrap().is_ascii_digit() {
        // Example: "5:hello" -> "hello"
        let colon_index = encoded_value.find(':').unwrap();
        let number_string = &encoded_value[..colon_index];
        let number = number_string.parse::<i64>().unwrap();
        let string = &encoded_value[colon_index + 1..colon_index + 1 + number as usize];

        serde_json::Value::String(string.to_string())

    } else if encoded_value.starts_with('i') {
        // If encoded_value starts with 'i', it's an integer
        // Example: "i45e" -> 45
        let end_index = encoded_value.find('e').unwrap();
        let int_as_string = &encoded_value[1..end_index];

        serde_json::Value::Number(int_as_string.parse::<i64>().unwrap().into())
    } else {
        panic!("Unhandled encoded value: {}", encoded_value)
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
            println!("{}", decoded_value);
        }

        _ => println!("unknown command: {}", args[1]),
    }

}
