use lopdf::Document;
use std::fs;

fn main() {
    let bytes = fs::read("input/input7.pdf").unwrap();
    println!("Read {} bytes", bytes.len());
    
    match Document::load_mem(&bytes) {
        Ok(doc) => {
            println!("Loaded from memory successfully!");
            println!("Pages: {:?}", doc.get_pages().len());
        }
        Err(e) => {
            println!("Error loading from memory: {:?}", e);
        }
    }
    
    match Document::load("input/input7.pdf") {
        Ok(doc) => {
            println!("Loaded from file successfully!");
            println!("Pages: {:?}", doc.get_pages().len());
        }
        Err(e) => {
            println!("Error loading from file: {:?}", e);
        }
    }
}
