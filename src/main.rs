use std::io::{prelude::*, BufReader};
use std::net::{TcpListener, TcpStream};
use std::collections::HashMap;
use crate::Body::{Single, MultiPart};

const KB: usize = 1024;
const MB: usize = KB * 1024;
const GB: usize = MB * 1024;
const LOCAL: &str = "127.0.0.1:7878";
const IPV4: &str = "45.77.158.123:7878";
const IPV6: &str = "[2001:19f0:5:5996&:5400:4ff:fe02:3d3e]:7878";
const MAX_REQ: usize = (64 * KB) - 1;

// NOTE: Receiving multipart/formdata is not currently supported.

/*
 * TODO
 * 
 * 1. Design a good way of organising these files so that they can be searched for on the website
 *    (Database of file names? Maybe. Sounds redundant. May be necessary).
 * 2. For now, this program will load the ENTIRE http request into memory. This drastically limits
 *      the size of requests you can process. This is okay since the size of requests are known
 *      beforehand. (The largest will be running PUT with a PDF file a few dozen KB in length). But
 *      I'd like to handle larger payloads using streams, buffers, file io, etc.
 * 3. TODO: Error, a crash can happen on a line with TODO: ERROR relating to headers without values
 * 4. TODO: Either read files in 8KB increments, or do not allow any HTTP requests larger than 8KB. 
 *      This prevents eating up all memory. It must be limited somehow to avoid a random 1GB PDF file 
 *      eating up all the memory.
 */

// Implement parsing of multipart/formdata 
// Implement extraction of PDFs
// Put them into a website
// Let you search by them
// Hugo tutorial

// The body of an HTTP request will be stored as either a raw of bytes, or a
// struct called FormData which is used as the value for a MultiPart enum. More
// info about FormData can be found in its definition.
#[derive(Debug)]
enum Body {
    Single(Vec<u8>), // Raw byte stream. Used when receiving raw data. TODO: This may be able to take a fixed size array instead of a Vector.
    MultiPart(Vec<FormData>) // Struct with headers. Used when receiving multipart data.
}

// When FormData is recieved in an HTTP request, each FormData contains
// headers and a body. There may be several of these FormDatas, each of which
// may contain another nested formdata entry. This is the recursive struct which defines
// them.
#[derive(Debug)]
struct FormData {
    headers: HashMap<String, String>,
    body: Body
}

// All relevant information in an HttpRequest
struct HttpRequest {
    method: String,
    location: String,
    version: String,
    headers: HashMap<String, String>,
    body: Body // Body is typically stored as a raw byte array
}

// Formats an HTTP response to an array of bytes.
macro_rules! response {
    ($response_code: expr, $message: expr) => {
        format!(
            "HTTP/1.1 {}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}\r\n",
            $response_code, $message.len() + 2, $message
        ).as_bytes()
    };
} 

// Finds the index of the first predicate (the byte to be searched for) in an
// array of bytes. Searches over a specified range. Returns None if the
// predicate cannot be found.
fn u8_index_of(array: &[u8], predicate: u8, start_index: usize, end_index: usize) -> Option<usize> {
    let index = array[start_index .. end_index]
        .iter()
        .position(|&pred| pred == predicate);
    if index.is_none() {
        return None;
    } else {
        let index = index.unwrap() + start_index;
        return Some(index)
    }
}

// Finds the index of the first predicate (the array of bytes to be searched
// for) in an array of bytes. Searches over a specified range. Returns None if
// the predicate cannot be found.
fn u8_index_of_multi(array: &[u8], predicate: &[u8], start_index: usize, end_index: usize) -> Option<usize> {
    let index = array[start_index .. end_index]
        .windows(predicate.len())
        .position(|pred| pred == predicate);
    if index.is_none() {
        return None;
    } else {
        let index = index.unwrap() + start_index;
        return Some(index);
    }
}


impl HttpRequest {
    // Returns a new HttpRequest struct from a well formatted HttpRequest as
    // bytes.
    // FIXME: Function assumes that HTTP requests are properly formed. 
    // NOTE: 'a is a lifetime indicator. It indicates that the lifetime of all
    // references with the generic type &'a are linked together. If one variable
    // is freed, the other will also be freed. (In this case, if
    // request_as_bytes no longer exists, then the error string will also not
    // exist).
    // TODO: I do not enjoy this functionality, but it is necessary (I believe)
    // in order to use the response! macro to generate responses at compile
    // time, when in reality, the http response code is not known until runtime.
    // Before this code goes live, I need to think about this more. This may be
    // able to be fixed without too much effort by using the ? symbol instead of
    // .unwrap().
    fn new<'a>(request_as_bytes: &'a[u8], mut stream: &TcpStream) -> Result<HttpRequest, &'a  str> {
        // Request line
        let request_size = request_as_bytes.len();
        if request_size == 0 {
            return Err("Http Request received with length 0");
        }
        let method_end = u8_index_of(request_as_bytes, b' ', 0, request_size).unwrap();
        let method = String::from_utf8_lossy(&request_as_bytes[..method_end]).into_owned();

        let location_end = u8_index_of(request_as_bytes, b' ', method_end + 1,  request_size);
        let request_line_end = u8_index_of(request_as_bytes, b'\r', method_end + 1, request_size)
            .expect("HTML is malformed");
        let location: String;
        let version: String;
        match location_end {
            None => {
                location = String::from_utf8_lossy(&request_as_bytes[method_end+1..request_line_end]).into_owned();
                version = format!("HTTP/1.0");
            },
            Some(location_end) => {
                location = String::from_utf8_lossy(&request_as_bytes[method_end+1..location_end]).into_owned();
                version = String::from_utf8_lossy(&request_as_bytes[location_end+1..request_line_end]).into_owned()
            }
        }

        // Headers
        let mut header_start = request_line_end + 2;
        match (version.as_str(), request_as_bytes[header_start] == b'\0') {
            ("HTTP/1.0", true) => {
                return Ok(
                    HttpRequest {
                        method: method,
                        location: location,
                        version: version,
                        headers: HashMap::new(),
                        body: Single(Vec::new()),
                    }
                );
            },
            (_, true) => {
                return Err("Request malformed. HTTP versions greated that 1.0 must contain a 'Host' header");
            },
            (_, false) => ()
        }
        let mut header_end = u8_index_of(request_as_bytes, b'\r', header_start, request_size)
            .expect("HTTP request is malformed. Each header must end with CRLF");
        let head_end = u8_index_of_multi(request_as_bytes, b"\r\n\r\n", header_end, request_size)
            .expect("HTTP request is malformed. Head must end with CRLF CRLF");
        let mut headers: HashMap<String, String> = HashMap::new();
        while header_start < head_end {
            let delim_pos = u8_index_of(request_as_bytes, b':', header_start, header_end)
                .expect("HTTP request is malformed. Every header must contain a key and a value separated by ':'");
            let header_key = &request_as_bytes[header_start..delim_pos];
            let mut header_value = &request_as_bytes[delim_pos+1..header_end]; //TODO: ERROR: This can break if there is no value, because it will save a 0 length byte array
            while header_value[0] == b' ' {
                header_value = &header_value[1..];
            }
            let header_key = String::from_utf8_lossy(header_key).to_ascii_lowercase();
            let header_value = String::from_utf8_lossy(header_value).into_owned();
            headers.insert(header_key, header_value);

            header_start = header_end + 2;
            header_end = u8_index_of(request_as_bytes, b'\r', header_start, head_end + 4)
                .expect("I don't know what went wrong");
        }

        // Get Body
        let body_start = head_end + 4;
        let body_in_buffer = body_start < request_size;
        let content_length = headers.get("content-length");
        let content_type = headers.get("content-type");
        //TODO: The below if statements are disgusting
        if let Some(content_type) = content_type {
            println!("There is a content_type");
            if content_type.starts_with("multipart") {
                return Err("Multipart data is not allowed at this time");
                /* Not gonna bother implementing multipart rn
                lazy_static! {
                    static ref RE_BOUNDARY: Regex = Regex::new("boundary=[\"]?(.*)[\"|;|\r]").unwrap();
                }
                let boundary = &RE_BOUNDARY.captures(&content_type).unwrap()[1];
                let boundary_as_bytes = boundary.as_bytes();
                let mut body = Vec::new();
                match (&content_length, &first_byte_of_body) {
                    (Some(length), b'\0') => {
                        if let Ok(length) = length.parse::<usize>() {
                            body.resize(length, b'\0');
                            let mut reader = BufReader::new(stream);
                            reader.read_exact(&mut body).unwrap();
                        } // The sender (client) should be informed of what's going on in the else case here. The request has a content length header that is not made up of entirely numbers
                    },
                    // The next two options are caused when there is no Content-Length header. IMO this is undefined behavior, and we should let the client know something is wrong
                    (None, b'\0') => {
                        // This may as well be an error
                        return Ok(HttpRequest{method, location, version, headers, body: MultiPart(Vec::new())})
                    },
                    (_, _) => {
                        body = request_as_bytes[body_start..].to_vec();
                    }
                }
                let mut after_boundary = boundary.len();
                let forms: Vec<FormData> = Vec::new();
                while &body[after_boundary..after_boundary+4] != b"--\r\n" {

                }
                return Ok(HttpRequest{method, location, version, headers, body: MultiPart(Vec::new())})
                */
            } else {
                let mut body = Vec::new();
                match (&content_length, body_in_buffer) {
                    (Some(length), false) => {
                        if let Ok(length) = length.parse::<usize>() {
                            body.resize(length, b'\0');
                            let mut bytes_read = stream.read(&mut body).unwrap();
                            while bytes_read > 0 && bytes_read < body.len() {
                                bytes_read = stream.read(&mut body).unwrap();
                            }
                            
                        } // The sender (client) should be informed of what's going on here. The request has a content length header that is not made up of entirely numbers
                    },
                    // The next two options are caused when there is no
                    // Content-Length header. For now this will be undefined
                    // behaviour, and the webserver will reject all requests
                    // without one. The webserver lets the client know that the
                    // request was invalid.
                    (None, false) => (),
                    (_, true) => {
                        println!("Body was in buffer");
                        body = request_as_bytes[body_start..].to_vec();
                    }
                };

                // Option 1
                // let mut body: Vec<u8> = Vec::new();
                // match (&content_length, body_in_buffer) {
                //     (Some(length), false) => {
                //         if let Ok(length) = length.parse::<usize>() {
                //             body.resize(length, b'\0');
                //             let mut reader = BufReader::new(stream);
                //             reader.read_exact(&mut body).unwrap();
                //         } // The sender (client) should be informed of what's going on here. The request has a content length header that is not made up of entirely numbers
                //     },
                //     // The next two options are caused when there is no Content-Length header. IMO this is undefined behavior, and we should let the client know something is wrong
                //     (None, false) => (),
                //     (_, true) => {
                //         body = request_as_bytes[body_start..].to_vec();
                //     }
                // };
                
                return Ok( HttpRequest {method, location, version, headers, body: Single(body)} );
            }
        } else {
            let mut body: Vec<u8> = Vec::new();
            match (&content_length, body_in_buffer) {
                (Some(length), false) => {
                    if let Ok(length) = length.parse::<usize>() {
                        body.resize(length, b'\0');
                        let mut reader = BufReader::new(stream);
                        reader.read_exact(&mut body).unwrap();
                    } // The sender (client) should be informed of what's going on here. The request has a content length header that is not made up of entirely numbers
                },
                // The next two options are caused when there is no Content-Length header. IMO this is undefined behavior, and we should let the client know something is wrong
                (None, false) => (),
                (_, true) => {
                    body = request_as_bytes[body_start..].to_vec();
                }
            };
            
            return Ok( HttpRequest {method, location, version, headers, body: Single(body)} );
        }
    }
}

// The main loop for the webserver
fn main() {
    
    let listener = TcpListener::bind(LOCAL).expect("Could not connect to server");
    
    
    // Listens for a connection
    for stream in listener.incoming() {
        
        // Max request (for now)
        let mut raw_request = [b'\0'; MAX_REQ]; // Max request size
        let mut stream = stream.unwrap();
        let request_size = stream.read(&mut raw_request).unwrap();
        println!("Request size: {}", request_size);

        if request_size > MAX_REQ {
            stream.write(
                response!("400 Bad Request", "Http Request is larger than max allowed request (64KB).")
            ).unwrap();
            stream.flush().unwrap();
            continue;
        } 

        let request = HttpRequest::new(&raw_request[..request_size], &stream);
        if let Err(error) = request {
            stream.write(response!("400 Bad Request", error)).unwrap();
            stream.flush().unwrap();
            continue;
        } 
                
        let request = request.unwrap();
        println!("\nMethod: {:#?}\nLocation: {:#?}\nVersion: {:#?}\nHeaders: {:#?}\nBody: {:?}\n", 
            request.method, request.location, request.version, request.headers, request.body);

        // Parse request
        match request.method.as_str() {
            "GET" => {
                stream.write(response!("200 OK", "GET REQUEST RECEIVED")).unwrap()
            }
            "POST" => {
                stream.write( response!("200 OK", "POST received")).unwrap()
            }
            _=> stream.write(response!("400 Bad Request", [request.method.as_str(), "is not supported yet"].join(" "))).unwrap() 
        };

        stream.flush().unwrap();
    }
}