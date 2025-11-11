// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::errors::*;
use crate::format::format_and_print;
use crate::options::*;
use crate::units::{Result, Unit};
use clap::{Arg, ArgAction, ArgMatches, Command, parser::ValueSource};
use std::cell::Cell;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::str::FromStr;

use units::{IEC_BASES, SI_BASES};
use uucore::display::Quotable;
use uucore::error::UResult;
use uucore::translate;

use uucore::parser::shortcut_value_parser::ShortcutValueParser;
use uucore::ranges::Range;
use uucore::{format_usage, show, show_error};

pub mod errors;
pub mod format;
pub mod options;
mod units;

fn normalize_arguments(args: impl uucore::Args) -> Vec<OsString> {
    args.map(|arg| {
        if arg == "---debug" || arg == "-debug" {
            OsString::from("--dev-debug")
        } else {
            arg
        }
    })
    .collect()
}

fn locale_supports_grouping() -> bool {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_NUMERIC"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let normalized = locale.trim().to_ascii_uppercase();
    !(normalized.is_empty() || normalized == "C" || normalized == "POSIX")
}

fn grouping_requested(options: &NumfmtOptions) -> bool {
    options.grouping || options.format.grouping
}

fn emit_debug_preamble(options: &NumfmtOptions, numbers_from_cli: bool) {
    if !options.debug {
        return;
    }

    if options.transform.from == Unit::None
        && options.transform.to == Unit::None
        && !options.grouping
        && !options.format_specified
        && options.padding == 0
    {
        show_error!("{}", translate!("numfmt-debug-no-conversion"));
    }

    if grouping_requested(options) && !locale_supports_grouping() {
        show_error!("{}", translate!("numfmt-debug-grouping-no-effect"));
    }

    if options.unit_separator.is_some() && options.delimiter.is_none() {
        show_error!("{}", translate!("numfmt-debug-field-delimiter-precedence"));
    }

    if options.header > 0 && numbers_from_cli {
        show_error!("{}", translate!("numfmt-debug-header-cli"));
    }
}

fn emit_debug_invalid_summary(options: &NumfmtOptions) {
    if options.debug && options.debug_invalid_encountered.get() {
        show_error!("{}", translate!("numfmt-debug-invalid-inputs"));
    }
}

fn handle_args<'a>(args: impl Iterator<Item = &'a str>, options: &NumfmtOptions) -> UResult<()> {
    for l in args {
        format_and_handle_validation(l, Some('\n'), options)?;
    }
    Ok(())
}

fn handle_buffer<R>(input: R, options: &NumfmtOptions) -> UResult<()>
where
    R: BufRead,
{
    if options.zero_terminated {
        handle_zero_terminated_buffer(input, options)
    } else {
        handle_newline_buffer(input, options)
    }
}

fn handle_newline_buffer<R>(mut input: R, options: &NumfmtOptions) -> UResult<()>
where
    R: BufRead,
{
    let mut buf = String::new();
    let mut idx = 0;

    loop {
        buf.clear();
        match input.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let terminator = if buf.ends_with('\n') {
                    buf.pop();
                    Some('\n')
                } else {
                    None
                };
                process_input_line(&buf, terminator, idx, options)?;
                idx += 1;
            }
            Err(err) => return Err(Box::new(NumfmtError::IoError(err.to_string()))),
        }
    }

    Ok(())
}

fn handle_zero_terminated_buffer<R>(mut input: R, options: &NumfmtOptions) -> UResult<()>
where
    R: BufRead,
{
    let mut buf = Vec::new();
    let mut idx = 0;

    loop {
        buf.clear();
        match input.read_until(0, &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let terminator = if buf.last() == Some(&0) {
                    buf.pop();
                    Some('\0')
                } else {
                    None
                };
                let line =
                    String::from_utf8(buf.clone()).expect("numfmt currently expects valid UTF-8");
                process_input_line(&line, terminator, idx, options)?;
                idx += 1;
            }
            Err(err) => return Err(Box::new(NumfmtError::IoError(err.to_string()))),
        }
    }

    Ok(())
}

fn process_input_line(
    line: &str,
    terminator: Option<char>,
    idx: usize,
    options: &NumfmtOptions,
) -> UResult<()> {
    if idx < options.header {
        print_with_terminator(line, terminator, options.zero_terminated);
        Ok(())
    } else {
        format_and_handle_validation(line, terminator, options)
    }
}

fn print_with_terminator(line: &str, terminator: Option<char>, zero_terminated: bool) {
    match terminator {
        Some(term) => print!("{line}{term}"),
        None if zero_terminated => print!("{line}"),
        None => println!("{line}"),
    }
}

fn format_and_handle_validation(
    input_line: &str,
    terminator: Option<char>,
    options: &NumfmtOptions,
) -> UResult<()> {
    let handled_line = format_and_print(input_line, options);

    if let Err(error_message) = handled_line {
        match options.invalid {
            InvalidModes::Abort => {
                return Err(Box::new(NumfmtError::FormattingError(error_message)));
            }
            InvalidModes::Fail => {
                options.debug_invalid_encountered.set(true);
                show!(NumfmtError::FormattingError(error_message));
            }
            InvalidModes::Warn => {
                options.debug_invalid_encountered.set(true);
                show_error!("{error_message}");
            }
            InvalidModes::Ignore => {
                options.debug_invalid_encountered.set(true);
            }
        }
        print_with_terminator(input_line, terminator, options.zero_terminated);
    }

    Ok(())
}

fn parse_unit(s: &str) -> Result<Unit> {
    match s {
        "auto" => Ok(Unit::Auto),
        "si" => Ok(Unit::Si),
        "iec" => Ok(Unit::Iec(false)),
        "iec-i" => Ok(Unit::Iec(true)),
        "none" => Ok(Unit::None),
        _ => Err(translate!("numfmt-error-unsupported-unit")),
    }
}

/// Parses a unit size. Suffixes are turned into their integer representations. For example, 'K'
/// will return `Ok(1000)`, and '2K' will return `Ok(2000)`.
fn parse_unit_size(s: &str) -> Result<usize> {
    let number: String = s.chars().take_while(char::is_ascii_digit).collect();
    let suffix = &s[number.len()..];

    if number.is_empty() || "0".repeat(number.len()) != number {
        if let Some(multiplier) = parse_unit_size_suffix(suffix) {
            if number.is_empty() {
                return Ok(multiplier);
            }

            if let Ok(n) = number.parse::<usize>() {
                return Ok(n * multiplier);
            }
        }
    }

    Err(translate!("numfmt-error-invalid-unit-size", "size" => s.quote()))
}

/// Parses a suffix of a unit size and returns the corresponding multiplier. For example,
/// the suffix 'K' will return `Some(1000)`, and 'Ki' will return `Some(1024)`.
///
/// If the suffix is empty, `Some(1)` is returned.
///
/// If the suffix is unknown, `None` is returned.
fn parse_unit_size_suffix(s: &str) -> Option<usize> {
    if s.is_empty() {
        return Some(1);
    }

    let suffix = s.chars().next().unwrap();

    if let Some(i) = ['K', 'M', 'G', 'T', 'P', 'E']
        .iter()
        .position(|&ch| ch == suffix)
    {
        return match s.len() {
            1 => Some(SI_BASES[i + 1] as usize),
            2 if s.ends_with('i') => Some(IEC_BASES[i + 1] as usize),
            _ => None,
        };
    }

    None
}

fn parse_options(args: &ArgMatches) -> Result<NumfmtOptions> {
    let from = parse_unit(args.get_one::<String>(FROM).unwrap())?;
    let to = parse_unit(args.get_one::<String>(TO).unwrap())?;
    let from_unit = parse_unit_size(args.get_one::<String>(FROM_UNIT).unwrap())?;
    let to_unit = parse_unit_size(args.get_one::<String>(TO_UNIT).unwrap())?;
    let grouping_flag = args.get_flag(GROUPING);
    let dev_debug_flag = args.get_flag(DEV_DEBUG);
    let debug_flag = args.get_flag(DEBUG) || dev_debug_flag;
    let format_specified = args.value_source(FORMAT) == Some(ValueSource::CommandLine);

    let transform = TransformOptions {
        from,
        from_unit,
        to,
        to_unit,
    };

    let padding = match args.get_one::<String>(PADDING) {
        Some(s) => s
            .parse::<isize>()
            .map_err(|_| s)
            .and_then(|n| match n {
                0 => Err(s),
                _ => Ok(n),
            })
            .map_err(|s| translate!("numfmt-error-invalid-padding", "value" => s.quote())),
        None => Ok(0),
    }?;

    let header = if args.value_source(HEADER) == Some(ValueSource::CommandLine) {
        let value = args.get_one::<String>(HEADER).unwrap();

        value
            .parse::<usize>()
            .map_err(|_| value)
            .and_then(|n| match n {
                0 => Err(value),
                _ => Ok(n),
            })
            .map_err(|value| translate!("numfmt-error-invalid-header", "value" => value.quote()))
    } else {
        Ok(0)
    }?;

    let fields = args.get_one::<String>(FIELD).unwrap().as_str();
    // a lone "-" means "all fields", even as part of a list of fields
    let fields = if fields.split(&[',', ' ']).any(|x| x == "-") {
        vec![Range {
            low: 1,
            high: usize::MAX,
        }]
    } else {
        Range::from_list(fields)?
    };

    let format = match args.get_one::<String>(FORMAT) {
        Some(s) => s.parse()?,
        None => FormatOptions::default(),
    };

    if grouping_flag && format_specified {
        return Err(translate!(
            "numfmt-error-grouping-cannot-be-combined-with-format"
        ));
    }

    if (grouping_flag || format.grouping) && to != Unit::None {
        return Err(translate!(
            "numfmt-error-grouping-cannot-be-combined-with-to"
        ));
    }

    let delimiter = args.get_one::<String>(DELIMITER).map_or(Ok(None), |arg| {
        if arg.len() <= 1 {
            Ok(Some(arg.to_owned()))
        } else {
            Err(translate!(
                "numfmt-error-delimiter-must-be-single-character"
            ))
        }
    })?;

    let unit_separator = args.get_one::<String>(UNIT_SEPARATOR).cloned();

    // unwrap is fine because the argument has a default value
    let round = match args.get_one::<String>(ROUND).unwrap().as_str() {
        "up" => RoundMethod::Up,
        "down" => RoundMethod::Down,
        "from-zero" => RoundMethod::FromZero,
        "towards-zero" => RoundMethod::TowardsZero,
        "nearest" => RoundMethod::Nearest,
        _ => unreachable!("Should be restricted by clap"),
    };

    let suffix = args.get_one::<String>(SUFFIX).cloned();

    let invalid = InvalidModes::from_str(args.get_one::<String>(INVALID).unwrap()).unwrap();

    let zero_terminated = args.get_flag(ZERO_TERMINATED);

    if debug_flag && padding != 0 {
        if let Some(pad) = format.padding {
            if !(format.zero_padding && pad > 0) {
                show_error!("{}", translate!("numfmt-debug-format-overrides-padding"));
            }
        }
    }

    Ok(NumfmtOptions {
        transform,
        padding,
        header,
        fields,
        delimiter,
        unit_separator,
        round,
        suffix,
        format,
        invalid,
        zero_terminated,
        grouping: grouping_flag,
        debug: debug_flag,
        dev_debug: dev_debug_flag,
        format_specified,
        debug_invalid_encountered: Cell::new(false),
    })
}

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let normalized_args = normalize_arguments(args);
    let matches =
        uucore::clap_localization::handle_clap_result(uu_app(), normalized_args.into_iter())?;

    let options = parse_options(&matches).map_err(NumfmtError::IllegalArgument)?;
    emit_debug_preamble(&options, matches.get_many::<String>(NUMBER).is_some());

    let result = match matches.get_many::<String>(NUMBER) {
        Some(values) => handle_args(values.map(|s| s.as_str()), &options),
        None => {
            let stdin = std::io::stdin();
            let mut locked_stdin = stdin.lock();
            handle_buffer(&mut locked_stdin, &options)
        }
    };

    emit_debug_invalid_summary(&options);

    match result {
        Err(e) => {
            std::io::stdout().flush().expect("error flushing stdout");
            Err(e)
        }
        _ => Ok(()),
    }
}

pub fn uu_app() -> Command {
    Command::new(uucore::util_name())
        .version(uucore::crate_version!())
        .help_template(uucore::localized_help_template(uucore::util_name()))
        .about(translate!("numfmt-about"))
        .after_help(translate!("numfmt-after-help"))
        .override_usage(format_usage(&translate!("numfmt-usage")))
        .allow_negative_numbers(true)
        .infer_long_args(true)
        .arg(
            Arg::new(DELIMITER)
                .short('d')
                .long(DELIMITER)
                .value_name("X")
                .help(translate!("numfmt-help-delimiter")),
        )
        .arg(
            Arg::new(FIELD)
                .long(FIELD)
                .help(translate!("numfmt-help-field"))
                .value_name("FIELDS")
                .allow_hyphen_values(true)
                .default_value(FIELD_DEFAULT),
        )
        .arg(
            Arg::new(FORMAT)
                .long(FORMAT)
                .help(translate!("numfmt-help-format"))
                .value_name("FORMAT")
                .allow_hyphen_values(true),
        )
        .arg(
            Arg::new(FROM)
                .long(FROM)
                .help(translate!("numfmt-help-from"))
                .value_name("UNIT")
                .default_value(FROM_DEFAULT),
        )
        .arg(
            Arg::new(FROM_UNIT)
                .long(FROM_UNIT)
                .help(translate!("numfmt-help-from-unit"))
                .value_name("N")
                .default_value(FROM_UNIT_DEFAULT),
        )
        .arg(
            Arg::new(TO)
                .long(TO)
                .help(translate!("numfmt-help-to"))
                .value_name("UNIT")
                .default_value(TO_DEFAULT),
        )
        .arg(
            Arg::new(TO_UNIT)
                .long(TO_UNIT)
                .help(translate!("numfmt-help-to-unit"))
                .value_name("N")
                .default_value(TO_UNIT_DEFAULT),
        )
        .arg(
            Arg::new(PADDING)
                .long(PADDING)
                .help(translate!("numfmt-help-padding"))
                .value_name("N"),
        )
        .arg(
            Arg::new(HEADER)
                .long(HEADER)
                .help(translate!("numfmt-help-header"))
                .num_args(..=1)
                .value_name("N")
                .default_missing_value(HEADER_DEFAULT)
                .hide_default_value(true),
        )
        .arg(
            Arg::new(ROUND)
                .long(ROUND)
                .help(translate!("numfmt-help-round"))
                .value_name("METHOD")
                .default_value("from-zero")
                .value_parser(ShortcutValueParser::new([
                    "up",
                    "down",
                    "from-zero",
                    "towards-zero",
                    "nearest",
                ])),
        )
        .arg(
            Arg::new(SUFFIX)
                .long(SUFFIX)
                .help(translate!("numfmt-help-suffix"))
                .value_name("SUFFIX"),
        )
        .arg(
            Arg::new(GROUPING)
                .long(GROUPING)
                .help(translate!("numfmt-help-grouping"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(INVALID)
                .long(INVALID)
                .help(translate!("numfmt-help-invalid"))
                .default_value("abort")
                .value_parser(["abort", "fail", "warn", "ignore"])
                .value_name("INVALID"),
        )
        .arg(
            Arg::new(DEBUG)
                .long(DEBUG)
                .help(translate!("numfmt-help-debug"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(DEV_DEBUG)
                .long("dev-debug")
                .hide(true)
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(ZERO_TERMINATED)
                .long(ZERO_TERMINATED)
                .short('z')
                .help(translate!("numfmt-help-zero-terminated"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(UNIT_SEPARATOR)
                .long(UNIT_SEPARATOR)
                .visible_alias("unit-sep")
                .value_name("SEP")
                .help(translate!("numfmt-help-unit-separator")),
        )
        .arg(Arg::new(NUMBER).hide(true).action(ArgAction::Append))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use uucore::error::get_exit_code;

    use super::{
        FormatOptions, InvalidModes, NumfmtOptions, Range, RoundMethod, TransformOptions, Unit,
        handle_args, handle_buffer, parse_unit_size, parse_unit_size_suffix,
    };
    use std::io::{BufReader, Error, ErrorKind, Read};
    struct MockBuffer {}

    impl Read for MockBuffer {
        fn read(&mut self, _: &mut [u8]) -> Result<usize, Error> {
            Err(Error::new(ErrorKind::BrokenPipe, "broken pipe"))
        }
    }

    fn get_valid_options() -> NumfmtOptions {
        NumfmtOptions {
            transform: TransformOptions {
                from: Unit::None,
                from_unit: 1,
                to: Unit::None,
                to_unit: 1,
            },
            padding: 10,
            header: 1,
            fields: vec![Range { low: 0, high: 1 }],
            delimiter: None,
            unit_separator: None,
            round: RoundMethod::Nearest,
            suffix: None,
            format: FormatOptions::default(),
            invalid: InvalidModes::Abort,
            zero_terminated: false,
            grouping: false,
            debug: false,
            dev_debug: false,
            format_specified: false,
            debug_invalid_encountered: Cell::new(false),
        }
    }

    #[test]
    fn broken_buffer_returns_io_error() {
        let mock_buffer = MockBuffer {};
        let result = handle_buffer(BufReader::new(mock_buffer), &get_valid_options())
            .expect_err("returned Ok after receiving IO error");
        let result_debug = format!("{result:?}");
        let result_display = format!("{result}");
        assert_eq!(result_debug, "IoError(\"broken pipe\")");
        assert_eq!(result_display, "broken pipe");
        assert_eq!(result.code(), 1);
    }

    #[test]
    fn broken_buffer_returns_io_error_after_header() {
        let mock_buffer = MockBuffer {};
        let mut options = get_valid_options();
        options.header = 0;
        let result = handle_buffer(BufReader::new(mock_buffer), &options)
            .expect_err("returned Ok after receiving IO error");
        let result_debug = format!("{result:?}");
        let result_display = format!("{result}");
        assert_eq!(result_debug, "IoError(\"broken pipe\")");
        assert_eq!(result_display, "broken pipe");
        assert_eq!(result.code(), 1);
    }

    #[test]
    fn non_numeric_returns_formatting_error() {
        let input_value = b"135\nhello";
        let result = handle_buffer(BufReader::new(&input_value[..]), &get_valid_options())
            .expect_err("returned Ok after receiving improperly formatted input");
        let result_debug = format!("{result:?}");
        let result_display = format!("{result}");
        assert_eq!(
            result_debug,
            "FormattingError(\"numfmt-error-invalid-number\")"
        );
        assert_eq!(result_display, "numfmt-error-invalid-number");
        assert_eq!(result.code(), 2);
    }

    #[test]
    fn valid_input_returns_ok() {
        let input_value = b"165\n100\n300\n500";
        let result = handle_buffer(BufReader::new(&input_value[..]), &get_valid_options());
        assert!(result.is_ok(), "did not return Ok for valid input");
    }

    #[test]
    fn warn_returns_ok_for_invalid_input() {
        let input_value = b"5\n4Q\n";
        let mut options = get_valid_options();
        options.invalid = InvalidModes::Warn;
        let result = handle_buffer(BufReader::new(&input_value[..]), &options);
        assert!(result.is_ok(), "did not return Ok for invalid input");
    }

    #[test]
    fn ignore_returns_ok_for_invalid_input() {
        let input_value = b"5\n4Q\n";
        let mut options = get_valid_options();
        options.invalid = InvalidModes::Ignore;
        let result = handle_buffer(BufReader::new(&input_value[..]), &options);
        assert!(result.is_ok(), "did not return Ok for invalid input");
    }

    #[test]
    fn buffer_fail_returns_status_2_for_invalid_input() {
        let input_value = b"5\n4Q\n";
        let mut options = get_valid_options();
        options.invalid = InvalidModes::Fail;
        handle_buffer(BufReader::new(&input_value[..]), &options).unwrap();
        assert_eq!(
            get_exit_code(),
            2,
            "should set exit code 2 for formatting errors"
        );
    }

    #[test]
    fn abort_returns_status_2_for_invalid_input() {
        let input_value = b"5\n4Q\n";
        let mut options = get_valid_options();
        options.invalid = InvalidModes::Abort;
        let result = handle_buffer(BufReader::new(&input_value[..]), &options);
        assert!(result.is_err(), "did not return err for invalid input");
    }

    #[test]
    fn args_fail_returns_status_2_for_invalid_input() {
        let input_value = ["5", "4Q"].into_iter();
        let mut options = get_valid_options();
        options.invalid = InvalidModes::Fail;
        handle_args(input_value, &options).unwrap();
        assert_eq!(
            get_exit_code(),
            2,
            "should set exit code 2 for formatting errors"
        );
    }

    #[test]
    fn args_warn_returns_status_0_for_invalid_input() {
        let input_value = ["5", "4Q"].into_iter();
        let mut options = get_valid_options();
        options.invalid = InvalidModes::Warn;
        let result = handle_args(input_value, &options);
        assert!(result.is_ok(), "did not return ok for invalid input");
    }

    #[test]
    fn test_parse_unit_size() {
        assert_eq!(1, parse_unit_size("1").unwrap());
        assert_eq!(1, parse_unit_size("01").unwrap());
        assert!(parse_unit_size("1.1").is_err());
        assert!(parse_unit_size("0").is_err());
        assert!(parse_unit_size("-1").is_err());
        assert!(parse_unit_size("A").is_err());
        assert!(parse_unit_size("18446744073709551616").is_err());
    }

    #[test]
    fn test_parse_unit_size_with_suffix() {
        assert_eq!(1000, parse_unit_size("K").unwrap());
        assert_eq!(1024, parse_unit_size("Ki").unwrap());
        assert_eq!(2000, parse_unit_size("2K").unwrap());
        assert_eq!(2048, parse_unit_size("2Ki").unwrap());
        assert!(parse_unit_size("0K").is_err());
    }

    #[test]
    fn test_parse_unit_size_suffix() {
        assert_eq!(1, parse_unit_size_suffix("").unwrap());
        assert_eq!(1000, parse_unit_size_suffix("K").unwrap());
        assert_eq!(1024, parse_unit_size_suffix("Ki").unwrap());
        assert_eq!(1000 * 1000, parse_unit_size_suffix("M").unwrap());
        assert_eq!(1024 * 1024, parse_unit_size_suffix("Mi").unwrap());
        assert!(parse_unit_size_suffix("Kii").is_none());
        assert!(parse_unit_size_suffix("A").is_none());
    }
}
