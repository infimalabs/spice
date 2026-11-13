use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::{
    DwarfLookup, LookupError, ParsedSample, SampleError, SampleTarget, SampledFrame,
    SourceLocation, parse_frame, parse_line_info, parse_lookup, parse_sample, resolve,
};

const IMAGE: &str = "spice";
const UUID: &str = "E300C974-4BA0-33DC-9CF8-A9B639019081";

/// The consumed lines of one `sample(1)` report, verbatim from a live capture.
const SAMPLE: &str = "\
Analysis of sampling spice (pid 44412) every 1 millisecond
Process:         spice [44412]
Path:            /work/*/spice
Load Address:    0x100604000
Identifier:      spice

Call graph:
    783 Thread_35643949   DispatchQueue_1: com.apple.main-thread  (serial)
    + 783 start  (in dyld) + 6992  [0x1888704e4]
    +   783 ???  (in spice)  load address 0x100604000 + 0x1bf2c8  [0x1007c32c8]
    +     783 ???  (in spice)  load address 0x100604000 + 0x515768  [0x100b19768]
    +       783 semaphore_wait_trap  (in libsystem_kernel.dylib) + 8  [0x188bf7bb0]
    783 Thread_35643980: spice-journal-writer
    + 783 thread_start  (in libsystem_pthread.dylib) + 8  [0x188c36c1c]
    +   783 ???  (in spice)  load address 0x100604000 + 0x31f236c  [0x1037f636c]
    + !   7 ???  (in spice)  load address 0x100604000 + 0x1bf2c8  [0x1007c32c8]

Total number in stack (recursive counted multiple, when >=5):
        7       ???  (in spice)  load address 0x100604000 + 0x31f236c  [0x1037f636c]

Sort by top of stack, same collapsed (when >= 5):
        ???  (in spice)        783
        semaphore_wait_trap  (in libsystem_kernel.dylib)        3132

Binary Images:
       0x100604000 -        0x105352b7b +spice (0) <E300C974-4BA0-33DC-9CF8-A9B639019081> /work/*/spice
       0x1887c8000 -        0x18881ab4b  libobjc.A.dylib (951.7) <03BD9E32-CF0A-37B0-898A-3CE8DE06D842> /usr/lib/libobjc.A.dylib

Sample analysis of process 44412 written to file /tmp/spice-sample.txt
";

/// One `dwarfdump --lookup --show-parents` report, verbatim from a live dSYM.
const LOOKUP: &str = "\
/tmp/spice.dSYM/Contents/Resources/DWARF/spice:\tfile format Mach-O arm64
0x006ac676: Compile Unit: length = 0x00000241, format = DWARF32, version = 0x0004, abbr_offset = 0x0000, addr_size = 0x08 (next unit at 0x006ac8bb)

0x006ac681: DW_TAG_compile_unit
              DW_AT_producer\t(\"clang LLVM (rustc version 1.97.1 (8bab26f4f 2026-07-14))\")
              DW_AT_language\t(DW_LANG_Rust)
              DW_AT_name\t(\"spice/src/lib.rs/@/buda2tc6srizzed9xyxniiqn7\")
              DW_AT_comp_dir\t(\"/work/spice\")
              DW_AT_low_pc\t(0x0000000101043530)
              DW_AT_high_pc\t(0x00000001010461f8)

0x006ac681:   DW_TAG_compile_unit
                DW_AT_name\t(\"spice/src/lib.rs/@/buda2tc6srizzed9xyxniiqn7\")
                DW_AT_comp_dir\t(\"/work/spice\")

0x006ac6a0:     DW_TAG_namespace
                  DW_AT_name\t(\"spice\")

0x006ac6a5:       DW_TAG_namespace
                    DW_AT_name\t(\"install\")

0x006ac6aa:         DW_TAG_namespace
                      DW_AT_name\t(\"symbols\")

0x006ac7b0:           DW_TAG_subprogram
                        DW_AT_low_pc\t(0x0000000101045370)
                        DW_AT_high_pc\t(0x00000001010458d0)
                        DW_AT_frame_base\t(DW_OP_reg29)
                        DW_AT_linkage_name\t(\"_RNvNtNtCseTbzhcbopZ4_5spice7install7symbols15executable_uuid\")
                        DW_AT_name\t(\"executable_uuid\")
                        DW_AT_decl_file\t(\"/work/spice/spice/src/install/symbols.rs\")
                        DW_AT_decl_line\t(325)
Line info: file 'symbols.rs', line 331, column 8, start file 'symbols.rs', start line 325
";

/// An unresolved address prints only the file-format header and exits zero.
const UNRESOLVED: &str =
    "/tmp/spice.dSYM/Contents/Resources/DWARF/spice:\tfile format Mach-O arm64\n";

#[test]
fn sample_parser_keeps_distinct_image_addresses_with_their_deepest_count() {
    let parsed = parse_sample(SAMPLE, IMAGE).expect("sample parses");
    assert_eq!(
        parsed,
        ParsedSample {
            load_address: 0x1_0060_4000,
            uuid: UUID.to_owned(),
            addresses: vec![
                (0x1_007c_32c8, 783),
                (0x1_00b1_9768, 783),
                (0x1_037f_636c, 783),
            ],
        }
    );
}

#[test]
fn sample_parser_names_each_missing_or_foreign_part() {
    let without_load = SAMPLE.replace("Load Address:    0x100604000\n", "");
    assert_eq!(
        parse_sample(&without_load, IMAGE),
        Err("names no load address")
    );
    let bad_load = SAMPLE.replace("Load Address:    0x100604000", "Load Address:    nope");
    assert_eq!(
        parse_sample(&bad_load, IMAGE),
        Err("load address is not hexadecimal")
    );
    assert_eq!(
        parse_sample(SAMPLE, "other"),
        Err("lists no binary image for the installed executable")
    );
    let without_images = SAMPLE.replace("Binary Images:", "Images:");
    assert_eq!(
        parse_sample(&without_images, IMAGE),
        Err("lists no binary images")
    );
    let moved = SAMPLE.replace(
        "0x100604000 -        0x105352b7b +spice",
        "0x100704000 -        0x105352b7b +spice",
    );
    assert_eq!(
        parse_sample(&moved, IMAGE),
        Err("lists the installed image at another address than it loaded")
    );
    let no_uuid = SAMPLE.replace(&format!("<{UUID}>"), "");
    assert_eq!(
        parse_sample(&no_uuid, IMAGE),
        Err("lists the installed image without a UUID")
    );
    let lowercase = SAMPLE.replace(UUID, &UUID.to_ascii_lowercase());
    assert_eq!(
        parse_sample(&lowercase, IMAGE),
        Err("lists a non-canonical UUID for the installed image")
    );
    let without_graph = SAMPLE.replace("Call graph:", "Graph:");
    assert_eq!(
        parse_sample(&without_graph, IMAGE),
        Err("carries no call graph")
    );
    let malformed = SAMPLE.replace("[0x1007c32c8]", "[0x1007c32c8");
    assert_eq!(
        parse_sample(&malformed, IMAGE),
        Err("carries a malformed frame line")
    );
    let only_foreign = SAMPLE.replace("(in spice)  load", "(in other)  load");
    assert_eq!(
        parse_sample(&only_foreign, IMAGE),
        Err("attributes no frame to the installed image")
    );
}

#[test]
fn frame_lines_read_the_first_count_and_the_bracketed_runtime_address() {
    assert_eq!(
        parse_frame(
            "    + ! : 12 ???  (in spice)  load address 0x100604000 + 0x1bf2c8  [0x1007c32c8]"
        ),
        Some((0x1_007c_32c8, 12))
    );
    assert_eq!(parse_frame("        ???  (in spice)        783"), None);
    assert_eq!(
        parse_frame("    + 783 start  (in dyld) + 6992  [0x1888704e4"),
        None
    );
    // The system symbolizer resolves a frame itself once a dSYM sits beside
    // the sampled image, appending a function name before the bracket and a
    // source location after it rather than leaving the bracket at the end.
    assert_eq!(
        parse_frame("    +     2573 spice::main  (in spice) + 2928  [0x1032b36c4]  main.rs:251"),
        Some((0x1032_b36c4, 2573))
    );
}

#[test]
fn lookup_parser_qualifies_the_subprogram_through_its_namespaces() {
    assert_eq!(
        parse_lookup(LOOKUP),
        Some(SampledFrame {
            function: "spice::install::symbols::executable_uuid".to_owned(),
            declared: SourceLocation {
                file: "/work/spice/spice/src/install/symbols.rs".to_owned(),
                line: 325,
                column: None,
            },
            line: Some(SourceLocation {
                file: "symbols.rs".to_owned(),
                line: 331,
                column: Some(8),
            }),
        })
    );
    assert_eq!(parse_lookup(UNRESOLVED), None);
    assert_eq!(parse_lookup(""), None);
}

#[test]
fn lookup_parser_drops_a_zero_line_and_ignores_inlined_callees() {
    let zero_line = LOOKUP.replace("line 331, column 8", "line 0, column 0");
    let frame = parse_lookup(&zero_line).expect("subprogram still resolves");
    assert_eq!(frame.line(), None);
    assert_eq!(frame.function(), "spice::install::symbols::executable_uuid");
    assert_eq!(frame.declared().line(), 325);
    let inlined = LOOKUP.replace(
        "Line info:",
        "0x006ac7f0:             DW_TAG_inlined_subroutine\n\
         \t\t\t\t\t\tDW_AT_abstract_origin\t(0x006ac900 \"_RNvXs_helper\")\n\
         \t\t\t\t\t\tDW_AT_name\t(\"helper\")\n\
         \t\t\t\t\t\tDW_AT_call_file\t(\"/x.rs\")\n\
         \t\t\t\t\t\tDW_AT_call_line\t(9)\n\
         Line info:",
    );
    let frame = parse_lookup(&inlined).expect("outer subprogram resolves");
    assert_eq!(frame.function(), "spice::install::symbols::executable_uuid");
    assert_eq!(
        frame.declared().file(),
        "/work/spice/spice/src/install/symbols.rs"
    );
    let nameless = LOOKUP.replace("DW_AT_name\t(\"executable_uuid\")\n", "");
    assert_eq!(parse_lookup(&nameless), None);
}

#[test]
fn line_info_parser_reads_file_line_and_column() {
    assert_eq!(
        parse_line_info(" file 'a.rs', line 7, column 3, start file 'a.rs', start line 5"),
        Some(SourceLocation {
            file: "a.rs".to_owned(),
            line: 7,
            column: Some(3),
        })
    );
    assert_eq!(
        parse_line_info(" file 'a.rs', line 7, column 0, start file 'a.rs', start line 5"),
        Some(SourceLocation {
            file: "a.rs".to_owned(),
            line: 7,
            column: None,
        })
    );
    assert_eq!(parse_line_info(" file 'a.rs', line x, column 0"), None);
}

/// Scripted lookups recording every address asked, for symbolizer tests.
struct ScriptedLookup {
    asked: RefCell<Vec<(PathBuf, u64)>>,
    reports: Vec<(u64, Result<String, LookupError>)>,
}

fn target(uuid: &str, text_address: Option<u64>) -> SampleTarget {
    SampleTarget {
        image: IMAGE.to_owned(),
        uuid: uuid.to_owned(),
        executable: ".spice-install/bin/spice".to_owned(),
        text_address,
        dwarf: PathBuf::from("/tmp/spice.dSYM/Contents/Resources/DWARF/spice"),
    }
}

#[test]
fn resolver_slides_each_address_into_the_file_and_names_what_dwarf_resolves() {
    let lookup = ScriptedLookup {
        asked: RefCell::new(Vec::new()),
        reports: vec![(0x1_001b_f2c8, Ok(LOOKUP.to_owned()))],
    };
    let report = resolve(
        Path::new("/tmp/s.txt"),
        SAMPLE,
        &target(UUID, Some(0x1_0000_0000)),
        &lookup,
    )
    .expect("sample resolves");
    assert_eq!(
        lookup.asked.borrow().as_slice(),
        [
            (
                PathBuf::from("/tmp/spice.dSYM/Contents/Resources/DWARF/spice"),
                0x1_001b_f2c8
            ),
            (
                PathBuf::from("/tmp/spice.dSYM/Contents/Resources/DWARF/spice"),
                0x1_0051_5768
            ),
            (
                PathBuf::from("/tmp/spice.dSYM/Contents/Resources/DWARF/spice"),
                0x1_031f_236c
            ),
        ]
    );
    assert_eq!((report.addresses(), report.resolved()), (3, 1));
    let first = &report.frames()[0];
    assert_eq!((first.address(), first.samples()), ("0x1007c32c8", 783));
    assert_eq!(first.file_address, "0x1001bf2c8");
    let frame = first.frame().expect("first frame resolves");
    assert_eq!(frame.function(), "spice::install::symbols::executable_uuid");
    assert_eq!(frame.line().map(SourceLocation::line), Some(331));
    assert_eq!(report.frames()[1].frame(), None);
    let json = serde_json::to_value(&report).expect("report serializes");
    assert_eq!(json["frames"][1].get("frame"), None);
    assert_eq!(json["frames"][0]["frame"]["declared"]["line"], 325);
    assert_eq!(json["frames"][0]["frame"]["line"]["column"], 8);
    assert_eq!(json["frames"][0]["frame"]["declared"].get("column"), None);
    assert_eq!(json["load_address"], "0x100604000");
    assert_eq!(json["uuid"], UUID);
}

#[test]
fn resolver_refuses_before_any_lookup_when_the_image_or_slide_disagree() {
    let lookup = ScriptedLookup {
        asked: RefCell::new(Vec::new()),
        reports: Vec::new(),
    };
    let other = "5919A3D6-1F5D-3A35-8342-DAA95D66F56E";
    let error = resolve(
        Path::new("/tmp/s.txt"),
        SAMPLE,
        &target(other, Some(0x1_0000_0000)),
        &lookup,
    )
    .expect_err("uuid mismatch refuses");
    assert!(
        matches!(error, SampleError::Uuid { sampled, installed } if sampled == UUID && installed == other)
    );
    let error = resolve(
        Path::new("/tmp/s.txt"),
        SAMPLE,
        &target(UUID, None),
        &lookup,
    )
    .expect_err("missing text segment refuses");
    assert!(matches!(error, SampleError::NoTextSegment(_)));
    let error = resolve(Path::new("/tmp/s.txt"), "", &target(UUID, None), &lookup)
        .expect_err("shape refuses");
    assert!(matches!(error, SampleError::Shape { .. }));
    assert!(lookup.asked.borrow().is_empty());
}

#[test]
fn resolver_reports_the_first_failing_lookup_with_its_runtime_address() {
    let lookup = ScriptedLookup {
        asked: RefCell::new(Vec::new()),
        reports: vec![(
            0x1_0051_5768,
            Err(LookupError("dwarfdump exceeded 5000ms".to_owned())),
        )],
    };
    let error = resolve(
        Path::new("/tmp/s.txt"),
        SAMPLE,
        &target(UUID, Some(0x1_0000_0000)),
        &lookup,
    )
    .expect_err("lookup failure surfaces");
    assert_eq!(
        error.to_string(),
        "lookup of 0x100b19768 failed: dwarfdump exceeded 5000ms"
    );
    assert_eq!(lookup.asked.borrow().len(), 2);
}

#[test]
fn resolver_marks_an_address_below_the_load_address_unmapped_without_a_lookup() {
    let lookup = ScriptedLookup {
        asked: RefCell::new(Vec::new()),
        reports: Vec::new(),
    };
    let text = SAMPLE.replace("[0x1007c32c8]", "[0x1000c32c8]");
    let report = resolve(
        Path::new("/tmp/s.txt"),
        &text,
        &target(UUID, Some(0x1_0000_0000)),
        &lookup,
    )
    .expect("sample resolves");
    assert_eq!(report.frames()[0].file_address, "unmapped");
    assert_eq!(report.frames()[0].frame(), None);
    assert_eq!(lookup.asked.borrow().len(), 2);
}

impl DwarfLookup for ScriptedLookup {
    fn lookup(&self, dwarf: &Path, address: u64) -> Result<String, LookupError> {
        self.asked.borrow_mut().push((dwarf.to_path_buf(), address));
        self.reports
            .iter()
            .find(|(expected, _)| *expected == address)
            .map_or_else(|| Ok(UNRESOLVED.to_owned()), |(_, report)| report.clone())
    }
}
