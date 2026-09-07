//! Streaming importer for the IMGpedia-shaped N-Triples/Turtle dumps.
//!
//! The input is a literal-free triple dump whose every term is a prefixed
//! image or property name:
//!
//! ```text
//! @prefix img: <https://www.imfd.cl/imgpedia/> .
//! img:100000001 img:P121 img:33992534 .
//! ```
//!
//! which maps onto a property graph as: one node per distinct image id,
//! carrying the label `img` and an integer property `id`; one directed
//! edge per triple, labelled by the predicate's local name (`P121`).
//!
//! ```text
//! import_ttl <graph.ttl> <out.gdb> [options]
//!
//!   --node-label <name>   label given to every node (default: img)
//!   --id-prop <name>      integer property holding the external id
//!                         (default: id)
//!   --max-edges <n>       stop after n triples (for sampling a big dump)
//!   --progress <n>        report every n input lines (default: 10000000)
//! ```
//!
//! # Why a dedicated binary
//!
//! The ordinary import path builds a whole `MemoryGraphStore` and calls
//! `save_graph`. That store keeps a `String` name, a `HashMap` of
//! properties and three adjacency `Vec`s per element — on the order of
//! 200 bytes an edge, which a half-billion-triple dump turns into
//! something no machine will hold. This binary never materialises an
//! element: it streams the file twice, writing records to pages as it
//! reads, and keeps only the columns the on-disk indexes are computed
//! from.
//!
//! Peak resident memory is roughly
//!
//! ```text
//!   20 bytes x edges  +  16 bytes x nodes
//! ```
//!
//! (`src`, `tgt` and the record locator per edge, the CSR arrays, and the
//! external-to-internal id map), so a 550 M-triple / 17 M-node dump wants
//! about 12 GiB. That is the price of the format's index writers taking
//! whole slices; the file itself is written incrementally.
//!
//! Edges carry no element name. Every edge interns the same empty string,
//! so the name table holds one entry for all of them rather than one per
//! edge — nothing in the query path resolves an edge name anyway.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use frogql::pager::page::{Page, PageType};
use frogql::pager::pager::Pager;
use frogql::store::disk_index;
use frogql::store::record::{self, PropValue};
use frogql::store::string_table::StringTable;

const READ_BUF: usize = 8 << 20;

struct Args {
    input: PathBuf,
    output: PathBuf,
    node_label: String,
    id_prop: String,
    max_edges: Option<u64>,
    progress: u64,
}

fn usage() -> ! {
    eprintln!(
        "usage: import_ttl <graph.ttl> <out.gdb> [options]\n\
         \n\
         options:\n  \
           --node-label <name>   label for every node (default: img)\n  \
           --id-prop <name>      integer property holding the external id (default: id)\n  \
           --max-edges <n>       stop after n triples\n  \
           --progress <n>        report every n lines (default: 10000000)"
    );
    process::exit(2)
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut node_label = "img".to_string();
    let mut id_prop = "id".to_string();
    let mut max_edges = None;
    let mut progress = 10_000_000u64;

    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].clone();
        let mut value = |name: &str| -> String {
            i += 1;
            match argv.get(i) {
                Some(v) => v.clone(),
                None => {
                    eprintln!("error: {name} needs a value");
                    usage()
                }
            }
        };
        match a.as_str() {
            "--node-label" => node_label = value("--node-label"),
            "--id-prop" => id_prop = value("--id-prop"),
            "--max-edges" => max_edges = Some(parse_u64(&value("--max-edges"), "--max-edges")),
            "--progress" => progress = parse_u64(&value("--progress"), "--progress"),
            "-h" | "--help" => usage(),
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                usage()
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    if positional.len() != 2 {
        eprintln!("error: expected an input .ttl and an output .gdb");
        usage();
    }
    Args {
        input: PathBuf::from(&positional[0]),
        output: PathBuf::from(&positional[1]),
        node_label,
        id_prop,
        max_edges,
        progress: progress.max(1),
    }
}

fn parse_u64(s: &str, flag: &str) -> u64 {
    match s.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("error: {flag} wants a non-negative integer, got `{s}`");
            process::exit(2)
        }
    }
}

// ---------------------------------------------------------------------------
// Triple parsing
// ---------------------------------------------------------------------------

/// One triple's terms, borrowed out of the line buffer.
struct Triple<'a> {
    subject: u32,
    predicate: &'a str,
    object: u32,
}

/// Strip a prefixed name down to its local part: `img:P121` -> `P121`,
/// `<https://host/hog>` -> `hog`. A bare token is returned as-is.
fn local_name(term: &str) -> &str {
    let t = term.trim();
    let t = t.strip_prefix('<').unwrap_or(t);
    let t = t.strip_suffix('>').unwrap_or(t);
    // Take everything after the last `:`, `/` or `#`.
    match t.rfind([':', '/', '#']) {
        Some(i) => &t[i + 1..],
        None => t,
    }
}

/// What one input line turned out to be.
///
/// The three outcomes are kept apart because only one of them is
/// harmless. A dropped triple is data the graph will not have, and a
/// query against it then returns fewer rows with nothing to say why —
/// so `Dropped` carries the reason and the caller reports it loudly.
enum Line<'a> {
    /// A directive, blank, or comment. Expected; not counted.
    Ignored,
    Triple(Triple<'a>),
    Dropped(&'static str),
}

/// Parse one `S P O .` line.
fn parse_triple(line: &str) -> Line<'_> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
        return Line::Ignored;
    }
    let line = line.strip_suffix('.').unwrap_or(line).trim_end();
    let mut parts = line.split_ascii_whitespace();
    let (s, p, o) = match (parts.next(), parts.next(), parts.next()) {
        (Some(s), Some(p), Some(o)) => (s, p, o),
        _ => return Line::Dropped("fewer than three terms"),
    };
    // An id wider than u32 is the failure mode worth naming: element ids
    // are `u32` throughout the engine, so such a triple cannot be stored
    // at all, and a dump carrying one would otherwise lose it in silence.
    let subject = match local_name(s).parse::<u32>() {
        Ok(v) => v,
        Err(_) => {
            return Line::Dropped(if local_name(s).bytes().all(|b| b.is_ascii_digit()) {
                "subject id does not fit in u32"
            } else {
                "subject is not a numeric image id"
            })
        }
    };
    let object = match local_name(o).parse::<u32>() {
        Ok(v) => v,
        Err(_) => {
            return Line::Dropped(if local_name(o).bytes().all(|b| b.is_ascii_digit()) {
                "object id does not fit in u32"
            } else {
                "object is not a numeric image id (a literal?)"
            })
        }
    };
    Line::Triple(Triple {
        subject,
        predicate: local_name(p),
        object,
    })
}

// ---------------------------------------------------------------------------
// A u32 set that does not pay SipHash on half a billion probes
// ---------------------------------------------------------------------------

/// Open-addressing set of `u32`, linear probing, power-of-two capacity.
/// `u32::MAX` is the empty sentinel, so that one id cannot be stored — a
/// graph never reaches it, and rejecting it explicitly is cheaper than
/// carrying an occupancy bitmap.
struct U32Set {
    slots: Vec<u32>,
    mask: usize,
    len: usize,
}

const EMPTY: u32 = u32::MAX;

impl U32Set {
    fn new() -> U32Set {
        U32Set {
            slots: vec![EMPTY; 1 << 16],
            mask: (1 << 16) - 1,
            len: 0,
        }
    }

    #[inline]
    fn hash(v: u32) -> usize {
        // Fibonacci hashing: one multiply, then take the high bits.
        (v as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) as usize >> 16
    }

    #[inline]
    fn insert(&mut self, v: u32) {
        if self.len * 4 >= self.slots.len() * 3 {
            self.grow();
        }
        let mut i = Self::hash(v) & self.mask;
        loop {
            let cur = self.slots[i];
            if cur == v {
                return;
            }
            if cur == EMPTY {
                self.slots[i] = v;
                self.len += 1;
                return;
            }
            i = (i + 1) & self.mask;
        }
    }

    fn grow(&mut self) {
        let fresh = vec![EMPTY; self.slots.len() * 2];
        let old = std::mem::replace(&mut self.slots, fresh);
        self.mask = self.slots.len() - 1;
        for v in old {
            if v == EMPTY {
                continue;
            }
            let mut i = Self::hash(v) & self.mask;
            while self.slots[i] != EMPTY {
                i = (i + 1) & self.mask;
            }
            self.slots[i] = v;
        }
    }

    /// Consume the set into an ascending list of its members. Sorting by
    /// external id makes the internal ids deterministic across runs.
    fn into_sorted_vec(self) -> Vec<u32> {
        let mut out: Vec<u32> = self.slots.into_iter().filter(|v| *v != EMPTY).collect();
        out.sort_unstable();
        out
    }
}

/// External id -> internal id, same layout, values alongside the keys.
struct U32Map {
    keys: Vec<u32>,
    vals: Vec<u32>,
    mask: usize,
}

impl U32Map {
    fn from_sorted(sorted: &[u32]) -> U32Map {
        let mut cap = 1usize << 16;
        while cap * 3 < sorted.len() * 4 {
            cap <<= 1;
        }
        let mut m = U32Map {
            keys: vec![EMPTY; cap],
            vals: vec![0; cap],
            mask: cap - 1,
        };
        for (internal, &external) in sorted.iter().enumerate() {
            let mut i = U32Set::hash(external) & m.mask;
            while m.keys[i] != EMPTY {
                i = (i + 1) & m.mask;
            }
            m.keys[i] = external;
            m.vals[i] = internal as u32;
        }
        m
    }

    #[inline]
    fn get(&self, k: u32) -> Option<u32> {
        let mut i = U32Set::hash(k) & self.mask;
        loop {
            let cur = self.keys[i];
            if cur == k {
                return Some(self.vals[i]);
            }
            if cur == EMPTY {
                return None;
            }
            i = (i + 1) & self.mask;
        }
    }
}

// ---------------------------------------------------------------------------
// Page-append writer
// ---------------------------------------------------------------------------

/// Appends cells to a chain of pages, keeping the open page in memory so
/// a record costs one write per full page rather than a read plus a write
/// per record.
struct CellWriter {
    page_type: PageType,
    page: Option<(u32, Page)>,
}

impl CellWriter {
    fn new(page_type: PageType) -> CellWriter {
        CellWriter {
            page_type,
            page: None,
        }
    }

    fn push(&mut self, pager: &mut Pager, cell: &[u8]) -> std::io::Result<(u32, u16)> {
        if let Some((pg, page)) = self.page.as_mut() {
            if let Some(slot) = page.insert_cell(cell) {
                return Ok((*pg, slot));
            }
            let (pg, page) = (*pg, page.clone());
            pager.write_page(pg, &page)?;
        }
        let pg = pager.allocate_page()?;
        let mut page = Page::new(self.page_type);
        let slot = page
            .insert_cell(cell)
            .expect("a record larger than one page");
        self.page = Some((pg, page));
        Ok((pg, slot))
    }

    fn flush(&mut self, pager: &mut Pager) -> std::io::Result<()> {
        if let Some((pg, page)) = self.page.take() {
            pager.write_page(pg, &page)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------

fn main() {
    let args = parse_args();
    let t0 = Instant::now();

    // --- Pass 1: distinct node ids and distinct predicates ---
    eprintln!("pass 1: scanning {} ...", args.input.display());
    let mut nodes = U32Set::new();
    let mut predicates: HashMap<String, ()> = HashMap::new();
    let mut triples: u64 = 0;
    let mut lines: u64 = 0;

    let mut dropped: u64 = 0;
    let mut drop_reasons: HashMap<&'static str, u64> = HashMap::new();
    let mut first_dropped: Option<String> = None;

    for_each_line(&args.input, args.progress, "pass 1", |line| {
        lines += 1;
        match parse_triple(line) {
            Line::Ignored => {}
            Line::Dropped(why) => {
                dropped += 1;
                *drop_reasons.entry(why).or_insert(0) += 1;
                if first_dropped.is_none() {
                    first_dropped = Some(line.trim().chars().take(120).collect());
                }
            }
            Line::Triple(t) => {
                if t.subject == EMPTY || t.object == EMPTY {
                    eprintln!("error: id {} collides with the empty sentinel", u32::MAX);
                    process::exit(1);
                }
                nodes.insert(t.subject);
                nodes.insert(t.object);
                if !predicates.contains_key(t.predicate) {
                    predicates.insert(t.predicate.to_string(), ());
                }
                triples += 1;
            }
        }
        match args.max_edges {
            Some(m) => triples < m,
            None => true,
        }
    });

    if dropped > 0 {
        // Loud, and never fatal: a dump may legitimately carry lines this
        // importer does not model. But the count has to be visible, or a
        // query short of rows has nothing to point at.
        eprintln!("warning: {dropped} line(s) dropped, none of them in the graph:");
        let mut reasons: Vec<(&&str, &u64)> = drop_reasons.iter().collect();
        reasons.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (why, n) in reasons {
            eprintln!("           {n:>12}  {why}");
        }
        if let Some(sample) = &first_dropped {
            eprintln!("         first: {sample}");
        }
    }

    let externals = nodes.into_sorted_vec();
    let node_count = externals.len();
    let edge_total = triples;
    eprintln!(
        "  {lines} lines, {edge_total} triples, {node_count} nodes, {} predicates ({:.1}s)",
        predicates.len(),
        t0.elapsed().as_secs_f64()
    );
    if node_count == 0 {
        eprintln!("error: no triples parsed; is this the right file?");
        process::exit(1);
    }
    if u32::try_from(node_count).is_err() || u32::try_from(edge_total).is_err() {
        eprintln!("error: this dump exceeds the u32 element-id space");
        process::exit(1);
    }
    let index = U32Map::from_sorted(&externals);

    // --- Open the output and write the node records ---
    if args.output.exists() {
        if let Err(e) = std::fs::remove_file(&args.output) {
            eprintln!("error: cannot replace {}: {e}", args.output.display());
            process::exit(1);
        }
    }
    let mut pager = match Pager::create(&args.output) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot create {}: {e}", args.output.display());
            process::exit(1);
        }
    };
    let mut strings = StringTable::new();
    strings.init(&mut pager).expect("string table");
    let mut names = StringTable::new_names();
    names.init(&mut pager).expect("name table");

    let node_label_sid = strings
        .intern(&args.node_label, &mut pager)
        .expect("intern label");
    let id_prop_sid = strings
        .intern(&args.id_prop, &mut pager)
        .expect("intern id prop");
    // One shared name for every edge: nothing resolves an edge name, and
    // interning half a billion of them would dwarf the graph itself.
    let edge_name_sid = names.intern("", &mut pager).expect("intern edge name");

    eprintln!("writing {node_count} node records ...");
    let t1 = Instant::now();
    let mut node_writer = CellWriter::new(PageType::NodeData);
    let mut node_locs: Vec<(u32, u16)> = Vec::with_capacity(node_count);
    let mut name_buf = String::new();
    for &external in &externals {
        name_buf.clear();
        use std::fmt::Write as _;
        let _ = write!(name_buf, "{external}");
        let name_sid = names.intern(&name_buf, &mut pager).expect("intern name");
        let cell = record::encode_node(
            name_sid,
            &[node_label_sid],
            &[(id_prop_sid, PropValue::Int(external as i64))],
        );
        node_locs.push(node_writer.push(&mut pager, &cell).expect("write node"));
    }
    node_writer.flush(&mut pager).expect("flush nodes");
    drop(externals);
    eprintln!("  nodes written in {:.1}s", t1.elapsed().as_secs_f64());

    // --- Pass 2: stream the edges straight onto pages ---
    eprintln!("pass 2: writing {edge_total} edge records ...");
    let t2 = Instant::now();
    let mut pred_sid: HashMap<String, u32> = HashMap::with_capacity(predicates.len());
    for name in predicates.keys() {
        let sid = strings.intern(name, &mut pager).expect("intern predicate");
        pred_sid.insert(name.clone(), sid);
    }
    drop(predicates);

    let cap = edge_total as usize;
    let mut edge_writer = CellWriter::new(PageType::EdgeData);
    let mut edge_locs: Vec<(u32, u16)> = Vec::with_capacity(cap);
    let mut edge_src: Vec<u32> = Vec::with_capacity(cap);
    let mut edge_tgt: Vec<u32> = Vec::with_capacity(cap);
    // Label -> edge ids, for the on-disk label index.
    let mut label_edges: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut written: u64 = 0;

    for_each_line(&args.input, args.progress, "pass 2", |line| {
        if let Line::Triple(t) = parse_triple(line) {
            let src = index.get(t.subject).expect("subject seen in pass 1");
            let tgt = index.get(t.object).expect("object seen in pass 1");
            let sid = *pred_sid.get(t.predicate).expect("predicate seen in pass 1");
            let cell = record::encode_edge(edge_name_sid, &[sid], &[], src, tgt, true);
            let loc = edge_writer.push(&mut pager, &cell).expect("write edge");
            label_edges.entry(sid).or_default().push(written as u32);
            edge_locs.push(loc);
            edge_src.push(src);
            edge_tgt.push(tgt);
            written += 1;
        }
        written < edge_total
    });
    edge_writer.flush(&mut pager).expect("flush edges");
    eprintln!("  edges written in {:.1}s", t2.elapsed().as_secs_f64());
    let edge_count = written as usize;

    // --- Indexes ---
    eprintln!("building indexes ...");
    let t3 = Instant::now();

    let node_label_entries: Vec<(u32, Vec<u32>)> =
        vec![(node_label_sid, (0..node_count as u32).collect())];
    let node_label_root =
        disk_index::write_label_index(&mut pager, &node_label_entries).expect("node label index");
    drop(node_label_entries);

    let edge_label_entries: Vec<(u32, Vec<u32>)> = label_edges.into_iter().collect();
    let edge_label_root =
        disk_index::write_label_index(&mut pager, &edge_label_entries).expect("edge label index");
    drop(edge_label_entries);

    // CSR adjacency, bucket-sorted: count, prefix-sum, place. Every edge
    // is directed, so the undirected arrays stay empty.
    let csr = |ends: &[u32]| -> (Vec<u32>, Vec<u32>) {
        let mut offsets = vec![0u32; node_count + 1];
        for &n in ends {
            offsets[n as usize + 1] += 1;
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        let mut flat = vec![0u32; ends.len()];
        let mut cursor = offsets[..node_count].to_vec();
        for (eid, &n) in ends.iter().enumerate() {
            let pos = cursor[n as usize] as usize;
            flat[pos] = eid as u32;
            cursor[n as usize] += 1;
        }
        (offsets, flat)
    };
    let (out_off, out_fl) = csr(&edge_src);
    let (in_off, in_fl) = csr(&edge_tgt);
    let und_off = vec![0u32; node_count + 1];
    let csr_root = disk_index::write_adjacency_csr(
        &mut pager,
        &out_off,
        &out_fl,
        &in_off,
        &in_fl,
        &und_off,
        &[],
    )
    .expect("csr adjacency");
    drop((out_off, out_fl, in_off, in_fl, und_off));

    let st_root =
        disk_index::write_u32_list(&mut pager, strings.page_numbers()).expect("string dir");
    let names_root =
        disk_index::write_u32_list(&mut pager, names.page_numbers()).expect("name dir");
    let node_locs_root = disk_index::write_node_locs(&mut pager, &node_locs).expect("node locs");
    drop(node_locs);
    let directed = vec![true; edge_count];
    let edge_topo_root =
        disk_index::write_edge_topo(&mut pager, &edge_locs, &edge_src, &edge_tgt, &directed)
            .expect("edge topo");

    pager.header.node_count = node_count as u32;
    pager.header.edge_count = edge_count as u32;
    pager.header.label_index_root = node_label_root;
    pager.header.edge_label_index_root = edge_label_root;
    pager.header.adjacency_root = 0;
    pager.header.csr_adjacency_root = csr_root;
    pager.header.string_table_root = st_root;
    pager.header.names_root = names_root;
    pager.header.node_locs_root = node_locs_root;
    pager.header.edge_topo_root = edge_topo_root;
    pager.write_header().expect("write header");
    eprintln!("  indexes written in {:.1}s", t3.elapsed().as_secs_f64());

    let bytes = std::fs::metadata(&args.output)
        .map(|m| m.len())
        .unwrap_or(0);
    println!(
        "wrote {} — {node_count} nodes, {edge_count} edges, {:.2} GiB, {:.1} bytes/edge, {:.1}s",
        args.output.display(),
        bytes as f64 / (1024.0 * 1024.0 * 1024.0),
        bytes as f64 / edge_count.max(1) as f64,
        t0.elapsed().as_secs_f64()
    );
}

/// Stream a file line by line. The callback returns `false` to stop.
fn for_each_line<F: FnMut(&str) -> bool>(path: &Path, progress: u64, tag: &str, mut f: F) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: cannot open {}: {e}", path.display());
            process::exit(1);
        }
    };
    let mut reader = BufReader::with_capacity(READ_BUF, file);
    let mut buf = String::new();
    let mut n: u64 = 0;
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("error: reading {}: {e}", path.display());
                process::exit(1);
            }
        }
        n += 1;
        if n % progress == 0 {
            eprintln!("  {tag}: {n} lines");
        }
        if !f(&buf) {
            break;
        }
    }
}
