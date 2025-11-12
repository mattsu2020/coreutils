// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.
// spell-checker:ignore powf undelimited
use std::io::{self, Write};
use uucore::display::Quotable;
use uucore::translate;

use crate::locale::NumericLocale;
use crate::options::{DelimiterKind, InvalidModes, NumfmtOptions, RoundMethod, TransformOptions};
use crate::units::{DisplayableSuffix, IEC_BASES, RawSuffix, Result, SI_BASES, Suffix, Unit};

/// Iterate over a line's fields, where each field is a contiguous sequence of
/// non-whitespace, optionally prefixed with one or more characters of leading
/// whitespace. Fields are returned as tuples of `(prefix, field)`.
///
/// # Examples:
///
/// ```
/// let mut fields = uu_numfmt::format::WhitespaceSplitter { s: Some("    1234 5") };
///
/// assert_eq!(Some(("    ", "1234")), fields.next());
/// assert_eq!(Some((" ", "5")), fields.next());
/// assert_eq!(None, fields.next());
/// ```
///
/// Delimiters are included in the results; `prefix` will be empty only for
/// the first field of the line (including the case where the input line is
/// empty):
///
/// ```
/// let mut fields = uu_numfmt::format::WhitespaceSplitter { s: Some("first second") };
///
/// assert_eq!(Some(("", "first")), fields.next());
/// assert_eq!(Some((" ", "second")), fields.next());
///
/// let mut fields = uu_numfmt::format::WhitespaceSplitter { s: Some("") };
///
/// assert_eq!(Some(("", "")), fields.next());
/// ```
pub struct WhitespaceSplitter<'a> {
    pub s: Option<&'a str>,
}

impl<'a> Iterator for WhitespaceSplitter<'a> {
    type Item = (&'a str, &'a str);

    /// Yield the next field in the input string as a tuple `(prefix, field)`.
    fn next(&mut self) -> Option<Self::Item> {
        let haystack = self.s?;

        let (prefix, field) = haystack.split_at(
            haystack
                .find(|c: char| !is_field_separator(c))
                .unwrap_or(haystack.len()),
        );

        let (field, rest) = field.split_at(
            field
                .find(|c: char| is_field_separator(c))
                .unwrap_or(field.len()),
        );

        self.s = if rest.is_empty() { None } else { Some(rest) };

        Some((prefix, field))
    }
}

fn is_non_breaking_space(c: char) -> bool {
    matches!(c, '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{2060}')
}

fn is_field_separator(c: char) -> bool {
    c.is_whitespace() && !is_non_breaking_space(c)
}

fn is_blank_or_nbsp(c: char) -> bool {
    if matches!(c, '\n' | '\r') {
        return false;
    }

    c.is_whitespace() || is_non_breaking_space(c)
}

fn is_newline_or_blank(c: char) -> bool {
    matches!(c, '\n' | '\r') || is_blank_or_nbsp(c)
}

fn consume_unit_separator<'a>(input: &'a str, unit_separator: Option<&str>) -> (&'a str, bool) {
    if let Some(sep) = unit_separator {
        if sep.is_empty() {
            return (input, true);
        }
        if let Some(stripped) = input.strip_prefix(sep) {
            return (stripped, true);
        }
    }

    if let Some(ch) = input.chars().next() {
        if is_blank_or_nbsp(ch) {
            return (&input[ch.len_utf8()..], true);
        }
    }

    (input, false)
}

fn strip_unit_separator_from_end<'a>(
    input: &'a str,
    unit_separator: Option<&str>,
) -> (&'a str, bool) {
    if let Some(sep) = unit_separator {
        if sep.is_empty() {
            return (input, true);
        }
        if let Some(stripped) = input.strip_suffix(sep) {
            return (stripped, true);
        }
    }

    if let Some(ch) = input.chars().next_back() {
        if is_blank_or_nbsp(ch) {
            let new_len = input.len() - ch.len_utf8();
            return (&input[..new_len], true);
        }
    }

    (input, false)
}

fn parse_suffix(
    s: &str,
    unit_separator: Option<&str>,
    locale: &NumericLocale,
) -> Result<(f64, Option<Suffix>)> {
    if s.is_empty() {
        return Err(translate!("numfmt-error-invalid-number-empty"));
    }

    let trimmed = s.trim_end_matches(is_newline_or_blank);
    if trimmed.is_empty() {
        return Err(translate!("numfmt-error-invalid-number-empty"));
    }

    if trimmed.eq_ignore_ascii_case("nan") {
        return Err(translate!("numfmt-error-invalid-suffix", "input" => s.quote()));
    }

    if trimmed
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic())
        .unwrap_or(false)
    {
        return Err(translate!("numfmt-error-invalid-number", "input" => s.quote()));
    }

    if let Some((consumed, normalized)) = parse_locale_numeric_prefix(trimmed, locale) {
        if trimmed[..consumed].ends_with('.')
            || trimmed[..consumed].ends_with(locale.decimal_point())
        {
            return Err(translate!("numfmt-error-invalid-number", "input" => s.quote()));
        }

        let number = normalized
            .parse::<f64>()
            .map_err(|_| translate!("numfmt-error-invalid-number", "input" => s.quote()))?;
        handle_suffix_after_number(s, trimmed, number, consumed, unit_separator, locale)
    } else {
        parse_suffix_with_explicit_suffix(trimmed, unit_separator, s, locale)
    }
}

fn parse_locale_numeric_prefix(s: &str, locale: &NumericLocale) -> Option<(usize, String)> {
    let len = s.len();
    let mut consumed = 0;
    let mut normalized = String::with_capacity(len);
    let mut has_digits = false;
    let mut seen_decimal = false;
    let mut seen_exp = false;
    let mut allow_sign = true;

    while consumed < len {
        if !locale.grouping_sep().is_empty() && !seen_decimal && !seen_exp {
            if s[consumed..].starts_with(locale.grouping_sep()) {
                consumed += locale.grouping_sep().len();
                continue;
            }
        }

        let current = s[consumed..].chars().next().unwrap();
        let ch_len = current.len_utf8();

        if allow_sign && matches!(current, '+' | '-') {
            normalized.push(current);
            consumed += ch_len;
            allow_sign = false;
            continue;
        }

        if current.is_ascii_digit() {
            normalized.push(current);
            consumed += ch_len;
            has_digits = true;
            allow_sign = false;
            continue;
        }

        if !seen_decimal && (current == '.' || current == locale.decimal_point()) {
            normalized.push('.');
            consumed += ch_len;
            seen_decimal = true;
            allow_sign = false;
            continue;
        }

        if !seen_exp && has_digits && matches!(current, 'e' | 'E') {
            let rest_slice = &s[consumed + ch_len..];
            if has_exponent_digits(rest_slice) {
                normalized.push(current);
                consumed += ch_len;
                seen_exp = true;
                allow_sign = true;
                continue;
            } else {
                break;
            }
        }

        if seen_exp && allow_sign && matches!(current, '+' | '-') {
            normalized.push(current);
            consumed += ch_len;
            allow_sign = false;
            continue;
        }

        break;
    }

    if !has_digits {
        return None;
    }

    Some((consumed, normalized))
}

fn has_exponent_digits(rest: &str) -> bool {
    let mut slice = rest;
    if let Some(first) = slice.chars().next() {
        if matches!(first, '+' | '-') {
            slice = &slice[first.len_utf8()..];
        }
    }
    slice
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
}

fn handle_suffix_after_number(
    original: &str,
    trimmed: &str,
    number: f64,
    consumed: usize,
    unit_separator: Option<&str>,
    locale: &NumericLocale,
) -> Result<(f64, Option<Suffix>)> {
    let mut rest = &trimmed[consumed..];

    if trimmed[..consumed].ends_with('.') || trimmed[..consumed].ends_with(locale.decimal_point()) {
        return Err(translate!("numfmt-error-invalid-number", "input" => original.quote()));
    }

    if rest.is_empty() {
        return Ok((number, None));
    }

    let (after_sep, _) = consume_unit_separator(rest, unit_separator);
    rest = after_sep;

    if rest.is_empty() {
        return Ok((number, None));
    }

    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return Ok((number, None));
    };

    let raw_suffix = RawSuffix::from_char(first);

    if raw_suffix.is_none() {
        let trimmed_rest = rest.trim_start_matches(is_newline_or_blank);
        if trimmed_rest.is_empty() {
            return Ok((number, None));
        }
        return Err(translate!("numfmt-error-invalid-suffix", "input" => original.quote()));
    }

    let raw_suffix = raw_suffix.unwrap();

    let mut consumed_len = first.len_utf8();
    let mut with_i = false;

    if rest[consumed_len..].starts_with('i') {
        with_i = true;
        consumed_len += 'i'.len_utf8();
    }

    let leftover = rest[consumed_len..].trim_start_matches(is_newline_or_blank);

    if !leftover.is_empty() {
        return Err(translate!(
            "numfmt-error-invalid-suffix-detail",
            "input" => original.quote(),
            "suffix" => leftover.quote()
        ));
    }

    Ok((number, Some((raw_suffix, with_i))))
}

fn parse_suffix_with_explicit_suffix(
    trimmed: &str,
    unit_separator: Option<&str>,
    original: &str,
    locale: &NumericLocale,
) -> Result<(f64, Option<Suffix>)> {
    let mut body = trimmed;
    let mut with_i = false;

    if body.ends_with('i') {
        with_i = true;
        body = &body[..body.len() - 'i'.len_utf8()];
        if body.is_empty() {
            return Err(translate!("numfmt-error-invalid-suffix", "input" => original.quote()));
        }
    }

    let Some(last_char) = body.chars().next_back() else {
        return Err(translate!("numfmt-error-invalid-number", "input" => original.quote()));
    };
    let Some(raw_suffix) = RawSuffix::from_char(last_char) else {
        return Err(translate!("numfmt-error-invalid-suffix", "input" => original.quote()));
    };

    let suffix_start = body.len() - last_char.len_utf8();
    let (number_part, _) = strip_unit_separator_from_end(&body[..suffix_start], unit_separator);

    if number_part.is_empty() {
        return Err(translate!("numfmt-error-invalid-number", "input" => original.quote()));
    }

    let number = parse_locale_number_full(number_part, locale, original)?;

    Ok((number, Some((raw_suffix, with_i))))
}

fn parse_locale_number_full(value: &str, locale: &NumericLocale, original: &str) -> Result<f64> {
    if let Some((consumed, normalized)) = parse_locale_numeric_prefix(value, locale) {
        if consumed != value.len() {
            return Err(translate!("numfmt-error-invalid-number", "input" => original.quote()));
        }
        if value.ends_with('.') || value.ends_with(locale.decimal_point()) {
            return Err(translate!("numfmt-error-invalid-number", "input" => original.quote()));
        }

        normalized
            .parse::<f64>()
            .map_err(|_| translate!("numfmt-error-invalid-number", "input" => original.quote()))
    } else {
        Err(translate!("numfmt-error-invalid-number", "input" => original.quote()))
    }
}

fn numeric_prefix_end(value: &str) -> usize {
    let mut end = 0;
    for (idx, ch) in value.char_indices() {
        if ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.') {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn apply_grouping(value: &str, locale: &NumericLocale) -> String {
    let grouping_sep = locale.grouping_sep();
    if grouping_sep.is_empty() {
        return value.to_owned();
    }

    let numeric_end = numeric_prefix_end(value);
    if numeric_end == 0 {
        return value.to_owned();
    }

    let (numeric_part, rest) = value.split_at(numeric_end);
    if numeric_part.is_empty() {
        return value.to_owned();
    }

    let (sign, digits) = if let Some(first) = numeric_part.chars().next() {
        if matches!(first, '+' | '-') {
            (Some(first), &numeric_part[first.len_utf8()..])
        } else {
            (None, numeric_part)
        }
    } else {
        (None, numeric_part)
    };

    let (integer_part, fractional_part) = digits.split_once('.').unwrap_or((digits, ""));
    let grouped_integer = group_integer_part(integer_part, grouping_sep);

    let mut grouped = String::with_capacity(numeric_part.len() + grouping_sep.len() * 4);
    if let Some(sign_char) = sign {
        grouped.push(sign_char);
    }
    grouped.push_str(&grouped_integer);
    if !fractional_part.is_empty() {
        grouped.push('.');
        grouped.push_str(fractional_part);
    }
    grouped.push_str(rest);

    grouped
}

fn group_integer_part(integer_part: &str, grouping_sep: &str) -> String {
    if integer_part.is_empty() {
        return String::new();
    }

    let mut groups = Vec::new();
    let mut chunk = String::new();
    let mut count = 0;

    for ch in integer_part.chars().rev() {
        chunk.push(ch);
        count += 1;
        if count == 3 {
            groups.push(chunk.chars().rev().collect::<String>());
            chunk.clear();
            count = 0;
        }
    }
    if !chunk.is_empty() {
        groups.push(chunk.chars().rev().collect::<String>());
    }

    groups.reverse();
    groups.join(grouping_sep)
}

fn localize_decimal_point(value: &str, locale: &NumericLocale) -> String {
    let decimal_char = locale.decimal_point();
    if decimal_char == '.' {
        return value.to_owned();
    }

    let numeric_end = numeric_prefix_end(value);
    if numeric_end == 0 {
        return value.to_owned();
    }

    let (numeric_part, rest) = value.split_at(numeric_end);
    if let Some(pos) = numeric_part.find('.') {
        let mut localized = String::with_capacity(value.len());
        localized.push_str(&numeric_part[..pos]);
        localized.push(decimal_char);
        localized.push_str(&numeric_part[pos + 1..]);
        localized.push_str(rest);
        localized
    } else {
        value.to_owned()
    }
}

/// Returns the implicit precision of a number, which is the count of digits after the dot. For
/// example, 1.23 has an implicit precision of 2.
fn parse_implicit_precision(s: &str) -> usize {
    match s.split_once('.') {
        Some((_, decimal_part)) => decimal_part
            .chars()
            .take_while(char::is_ascii_digit)
            .count(),
        None => 0,
    }
}

fn remove_suffix(i: f64, s: Option<Suffix>, u: &Unit) -> Result<f64> {
    match (s, u) {
        (Some((raw_suffix, false)), &Unit::Auto | &Unit::Si) => match raw_suffix {
            RawSuffix::K => Ok(i * 1e3),
            RawSuffix::M => Ok(i * 1e6),
            RawSuffix::G => Ok(i * 1e9),
            RawSuffix::T => Ok(i * 1e12),
            RawSuffix::P => Ok(i * 1e15),
            RawSuffix::E => Ok(i * 1e18),
            RawSuffix::Z => Ok(i * 1e21),
            RawSuffix::Y => Ok(i * 1e24),
            RawSuffix::R => Ok(i * SI_BASES[9]),
            RawSuffix::Q => Ok(i * SI_BASES[10]),
        },
        (Some((raw_suffix, false)), &Unit::Iec(false))
        | (Some((raw_suffix, true)), &Unit::Auto | &Unit::Iec(true)) => match raw_suffix {
            RawSuffix::K => Ok(i * IEC_BASES[1]),
            RawSuffix::M => Ok(i * IEC_BASES[2]),
            RawSuffix::G => Ok(i * IEC_BASES[3]),
            RawSuffix::T => Ok(i * IEC_BASES[4]),
            RawSuffix::P => Ok(i * IEC_BASES[5]),
            RawSuffix::E => Ok(i * IEC_BASES[6]),
            RawSuffix::Z => Ok(i * IEC_BASES[7]),
            RawSuffix::Y => Ok(i * IEC_BASES[8]),
            RawSuffix::R => Ok(i * IEC_BASES[9]),
            RawSuffix::Q => Ok(i * IEC_BASES[10]),
        },
        (Some((raw_suffix, false)), &Unit::Iec(true)) => Err(
            translate!("numfmt-error-missing-i-suffix", "number" => i, "suffix" => format!("{raw_suffix:?}")),
        ),
        (Some((raw_suffix, with_i)), &Unit::None) => Err(
            translate!("numfmt-error-rejecting-suffix", "number" => i, "suffix" => format!("{raw_suffix:?}{}", if with_i { "i" } else { "" })),
        ),
        (None, _) => Ok(i),
        (_, _) => Err(translate!("numfmt-error-suffix-unsupported-for-unit")),
    }
}

fn transform_from(
    s: &str,
    opts: &TransformOptions,
    unit_separator: Option<&str>,
    locale: &NumericLocale,
) -> Result<f64> {
    let (i, suffix) = parse_suffix(s, unit_separator, locale)?;
    let i = i * (opts.from_unit as f64);

    remove_suffix(i, suffix, &opts.from).map(|n| {
        // GNU numfmt doesn't round values if no --from argument is provided by the user
        if opts.from == Unit::None {
            if n == -0.0 { 0.0 } else { n }
        } else if n < 0.0 {
            -n.abs().ceil()
        } else {
            n.ceil()
        }
    })
}

/// Divide numerator by denominator, with rounding.
///
/// If the result of the division is less than 10.0, round to one decimal point.
///
/// Otherwise, round to an integer.
///
/// # Examples:
///
/// ```
/// use uu_numfmt::format::div_round;
/// use uu_numfmt::options::RoundMethod;
///
/// // Rounding methods:
/// assert_eq!(div_round(1.01, 1.0, RoundMethod::FromZero), 1.1);
/// assert_eq!(div_round(1.01, 1.0, RoundMethod::TowardsZero), 1.0);
/// assert_eq!(div_round(1.01, 1.0, RoundMethod::Up), 1.1);
/// assert_eq!(div_round(1.01, 1.0, RoundMethod::Down), 1.0);
/// assert_eq!(div_round(1.01, 1.0, RoundMethod::Nearest), 1.0);
///
/// // Division:
/// assert_eq!(div_round(999.1, 1000.0, RoundMethod::FromZero), 1.0);
/// assert_eq!(div_round(1001., 10., RoundMethod::FromZero), 101.);
/// assert_eq!(div_round(9991., 10., RoundMethod::FromZero), 1000.);
/// assert_eq!(div_round(-12.34, 1.0, RoundMethod::FromZero), -13.0);
/// assert_eq!(div_round(1000.0, -3.14, RoundMethod::FromZero), -319.0);
/// assert_eq!(div_round(-271828.0, -271.0, RoundMethod::FromZero), 1004.0);
/// ```
pub fn div_round(n: f64, d: f64, method: RoundMethod) -> f64 {
    let v = n / d;

    if v.abs() < 10.0 {
        method.round(10.0 * v) / 10.0
    } else {
        method.round(v)
    }
}

/// Rounds to the specified number of decimal points.
fn round_with_precision(n: f64, method: RoundMethod, precision: usize) -> f64 {
    let p = 10.0_f64.powf(precision as f64);

    method.round(p * n) / p
}

fn consider_suffix(
    n: f64,
    u: &Unit,
    round_method: RoundMethod,
    precision: usize,
) -> Result<(f64, Option<Suffix>)> {
    let abs_n = n.abs();
    let suffixes = RawSuffix::ORDERED;

    let (bases, with_i) = match *u {
        Unit::Si => (&SI_BASES, false),
        Unit::Iec(with_i) => (&IEC_BASES, with_i),
        Unit::Auto => return Err(translate!("numfmt-error-unit-auto-not-supported-with-to")),
        Unit::None => return Ok((n, None)),
    };

    if abs_n <= bases[1] - 1.0 {
        return Ok((n, None));
    }

    let suffix_count = suffixes.len();
    if abs_n >= bases[suffix_count + 1] {
        return Err(translate!("numfmt-error-number-too-big"));
    }

    let mut i = 1;
    while i < suffix_count && abs_n >= bases[i + 1] {
        i += 1;
    }

    let v = if precision > 0 {
        round_with_precision(n / bases[i], round_method, precision)
    } else {
        div_round(n, bases[i], round_method)
    };

    // check if rounding pushed us into the next base
    if v.abs() >= bases[1] {
        if i == suffix_count {
            Err(translate!("numfmt-error-number-too-big"))
        } else {
            Ok((v / bases[1], Some((suffixes[i], with_i))))
        }
    } else {
        Ok((v, Some((suffixes[i - 1], with_i))))
    }
}

fn transform_to(
    s: f64,
    opts: &TransformOptions,
    round_method: RoundMethod,
    precision: usize,
    unit_separator: Option<&str>,
) -> Result<String> {
    let (i2, s) = consider_suffix(s, &opts.to, round_method, precision)?;
    let i2 = i2 / (opts.to_unit as f64);
    let separator = unit_separator.unwrap_or("");
    Ok(match s {
        None => {
            format!(
                "{:.precision$}",
                round_with_precision(i2, round_method, precision),
            )
        }
        Some(s) if precision > 0 => {
            format!(
                "{i2:.precision$}{separator}{}",
                DisplayableSuffix(s, opts.to),
            )
        }
        Some(s) if i2.abs() < 10.0 => {
            format!("{i2:.1}{separator}{}", DisplayableSuffix(s, opts.to))
        }
        Some(s) => format!("{i2:.0}{separator}{}", DisplayableSuffix(s, opts.to)),
    })
}

fn format_string(
    source: &str,
    options: &NumfmtOptions,
    implicit_padding: Option<isize>,
) -> Result<String> {
    // strip the (optional) suffix before applying any transformation
    let source_without_suffix = match &options.suffix {
        Some(suffix) => source.strip_suffix(suffix).unwrap_or(source),
        None => source,
    };

    let precision = if let Some(p) = options.format.precision {
        p
    } else if options.transform.from == Unit::None && options.transform.to == Unit::None {
        parse_implicit_precision(source_without_suffix)
    } else {
        0
    };

    let unit_separator = options.unit_separator.as_deref();

    let number = transform_to(
        transform_from(
            source_without_suffix,
            &options.transform,
            unit_separator,
            &options.numeric_locale,
        )?,
        &options.transform,
        options.round,
        precision,
        unit_separator,
    )?;

    let grouped_number = if options.grouping || options.format.grouping {
        apply_grouping(&number, &options.numeric_locale)
    } else {
        number
    };

    let localized_number = localize_decimal_point(&grouped_number, &options.numeric_locale);

    // bring back the suffix before applying padding
    let number_with_suffix = match &options.suffix {
        Some(suffix) => format!("{localized_number}{suffix}"),
        None => localized_number,
    };

    let padding = options
        .format
        .padding
        .unwrap_or_else(|| implicit_padding.unwrap_or(options.padding));

    let padded_number = match padding {
        0 => number_with_suffix,
        p if p > 0 && options.format.zero_padding => {
            let zero_padded = pad_with_width(&number_with_suffix, p as usize, false, '0');
            match implicit_padding.unwrap_or(options.padding) {
                0 => zero_padded,
                extra if extra > 0 => pad_with_width(&zero_padded, extra as usize, false, ' '),
                extra => pad_with_width(&zero_padded, width_from_isize(extra), true, ' '),
            }
        }
        p if p > 0 => pad_with_width(&number_with_suffix, p as usize, false, ' '),
        p => pad_with_width(&number_with_suffix, width_from_isize(p), true, ' '),
    };

    Ok(format!(
        "{}{padded_number}{}",
        options.format.prefix, options.format.suffix
    ))
}

fn unicode_delimiter(options: &NumfmtOptions) -> &str {
    match options
        .delimiter
        .as_ref()
        .expect("delimiter should be present for unicode variant")
    {
        DelimiterKind::Unicode(text) => text,
        DelimiterKind::Bytes(_) => unreachable!("expected unicode delimiter, found byte delimiter"),
    }
}

fn format_and_print_delimited_buffered(s: &str, options: &NumfmtOptions) -> Result<()> {
    let delimiter = unicode_delimiter(options);
    let mut output = String::with_capacity(s.len() + delimiter.len());

    for (n, field) in (1..).zip(s.split(delimiter)) {
        let field_selected = uucore::ranges::contain(&options.fields, n);

        if n > 1 {
            output.push_str(delimiter);
        }

        if field_selected {
            output.push_str(&format_string(field.trim_start(), options, None)?);
        } else {
            output.push_str(field);
        }
    }

    output.push(line_terminator(options));
    print!("{output}");

    Ok(())
}

fn format_and_print_undelimited_buffered(s: &str, options: &NumfmtOptions) -> Result<()> {
    let field_selected = uucore::ranges::contain(&options.fields, 1);
    let mut output = String::with_capacity(s.len() + 1);

    if field_selected {
        output.push_str(&format_string(s, options, None)?);
    } else {
        output.push_str(s);
    }

    output.push(line_terminator(options));
    print!("{output}");

    Ok(())
}

fn format_and_print_whitespace_buffered(s: &str, options: &NumfmtOptions) -> Result<()> {
    let mut output = String::with_capacity(s.len() + 1);

    for (n, (prefix, field)) in (1..).zip(WhitespaceSplitter { s: Some(s) }) {
        let field_selected = uucore::ranges::contain(&options.fields, n);

        if field_selected {
            let empty_prefix = prefix.is_empty();
            let prefix = if n > 1 {
                output.push(' ');
                &prefix[1..]
            } else {
                prefix
            };

            let implicit_padding = if !empty_prefix && options.padding == 0 {
                Some((prefix.len() + field.len()) as isize)
            } else {
                None
            };

            output.push_str(&format_string(field, options, implicit_padding)?);
        } else {
            let prefix = if options.zero_terminated && prefix.starts_with('\n') {
                output.push(' ');
                &prefix[1..]
            } else {
                prefix
            };
            output.push_str(prefix);
            output.push_str(field);
        }
    }

    output.push(line_terminator(options));
    print!("{output}");

    Ok(())
}

fn line_terminator(options: &NumfmtOptions) -> char {
    if options.zero_terminated { '\0' } else { '\n' }
}

fn format_and_print_delimited_streaming(s: &str, options: &NumfmtOptions) -> Result<()> {
    let delimiter = unicode_delimiter(options);

    for (n, field) in (1..).zip(s.split(delimiter)) {
        let field_selected = uucore::ranges::contain(&options.fields, n);

        if n > 1 {
            print!("{delimiter}");
        }

        if field_selected {
            print!("{}", format_string(field.trim_start(), options, None)?);
        } else {
            print!("{field}");
        }
    }

    print!("{}", line_terminator(options));

    Ok(())
}

fn format_and_print_undelimited_streaming(s: &str, options: &NumfmtOptions) -> Result<()> {
    let field_selected = uucore::ranges::contain(&options.fields, 1);

    if field_selected {
        print!("{}", format_string(s, options, None)?);
    } else {
        print!("{s}");
    }

    print!("{}", line_terminator(options));

    Ok(())
}

fn format_and_print_whitespace_streaming(s: &str, options: &NumfmtOptions) -> Result<()> {
    for (n, (prefix, field)) in (1..).zip(WhitespaceSplitter { s: Some(s) }) {
        let field_selected = uucore::ranges::contain(&options.fields, n);

        if field_selected {
            let empty_prefix = prefix.is_empty();

            let prefix = if n > 1 {
                print!(" ");
                &prefix[1..]
            } else {
                prefix
            };

            let implicit_padding = if !empty_prefix && options.padding == 0 {
                Some((prefix.len() + field.len()) as isize)
            } else {
                None
            };

            print!("{}", format_string(field, options, implicit_padding)?);
        } else {
            let prefix = if options.zero_terminated && prefix.starts_with('\n') {
                print!(" ");
                &prefix[1..]
            } else {
                prefix
            };
            print!("{prefix}{field}");
        }
    }

    print!("{}", line_terminator(options));

    Ok(())
}

fn format_and_print_byte_delimited_streaming(
    s: &str,
    delimiter: u8,
    options: &NumfmtOptions,
) -> Result<()> {
    let mut stdout = io::stdout();

    for (n, field_bytes) in s.as_bytes().split(|b| *b == delimiter).enumerate() {
        if n > 0 {
            stdout.write_all(&[delimiter]).map_err(|e| e.to_string())?;
        }

        let field_selected = uucore::ranges::contain(&options.fields, n + 1);
        if field_selected {
            let text = std::str::from_utf8(field_bytes)
                .map_err(|_| invalid_number_from_bytes(field_bytes))?;
            let formatted = format_string(text.trim_start(), options, None)?;
            stdout
                .write_all(formatted.as_bytes())
                .map_err(|e| e.to_string())?;
        } else {
            stdout.write_all(field_bytes).map_err(|e| e.to_string())?;
        }
    }

    stdout
        .write_all(&[line_terminator(options) as u8])
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn format_and_print_byte_delimited_buffered(
    s: &str,
    delimiter: u8,
    options: &NumfmtOptions,
) -> Result<()> {
    let mut buffer = Vec::with_capacity(s.len() + 1);

    for (n, field_bytes) in s.as_bytes().split(|b| *b == delimiter).enumerate() {
        if n > 0 {
            buffer.push(delimiter);
        }

        let field_selected = uucore::ranges::contain(&options.fields, n + 1);
        if field_selected {
            let text = std::str::from_utf8(field_bytes)
                .map_err(|_| invalid_number_from_bytes(field_bytes))?;
            let formatted = format_string(text.trim_start(), options, None)?;
            buffer.extend_from_slice(formatted.as_bytes());
        } else {
            buffer.extend_from_slice(field_bytes);
        }
    }

    buffer.push(line_terminator(options) as u8);
    io::stdout().write_all(&buffer).map_err(|e| e.to_string())?;

    Ok(())
}

fn pad_with_width(value: &str, width: usize, align_left: bool, pad_char: char) -> String {
    let current_width = value.chars().count();
    if width <= current_width {
        return value.to_owned();
    }

    let pad_len = width - current_width;
    let mut result = String::with_capacity(value.len() + pad_len * pad_char.len_utf8());

    if align_left {
        result.push_str(value);
        for _ in 0..pad_len {
            result.push(pad_char);
        }
    } else {
        for _ in 0..pad_len {
            result.push(pad_char);
        }
        result.push_str(value);
    }

    result
}

fn width_from_isize(value: isize) -> usize {
    if value >= 0 {
        value as usize
    } else if value == isize::MIN {
        (isize::MAX as usize).saturating_add(1)
    } else {
        (-value) as usize
    }
}

fn invalid_number_from_bytes(bytes: &[u8]) -> String {
    let display = String::from_utf8_lossy(bytes).into_owned();
    translate!("numfmt-error-invalid-number", "input" => display.quote())
}

/// Format a line of text according to the selected options.
///
/// Given a line of text `s`, split the line into fields, transform and format
/// any selected numeric fields, and print the result to stdout. Fields not
/// selected for conversion are passed through unmodified.
pub fn format_and_print(s: &str, options: &NumfmtOptions) -> Result<()> {
    let use_buffered = !matches!(options.invalid, InvalidModes::Abort);

    match (use_buffered, &options.delimiter) {
        (true, Some(DelimiterKind::Unicode(delim))) if delim.is_empty() => {
            format_and_print_undelimited_buffered(s, options)
        }
        (true, Some(DelimiterKind::Unicode(_))) => format_and_print_delimited_buffered(s, options),
        (true, Some(DelimiterKind::Bytes(bytes))) => {
            format_and_print_byte_delimited_buffered(s, bytes[0], options)
        }
        (true, None) => format_and_print_whitespace_buffered(s, options),
        (false, Some(DelimiterKind::Unicode(delim))) if delim.is_empty() => {
            format_and_print_undelimited_streaming(s, options)
        }
        (false, Some(DelimiterKind::Unicode(_))) => {
            format_and_print_delimited_streaming(s, options)
        }
        (false, Some(DelimiterKind::Bytes(bytes))) => {
            format_and_print_byte_delimited_streaming(s, bytes[0], options)
        }
        (false, None) => format_and_print_whitespace_streaming(s, options),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale;

    #[test]
    #[allow(clippy::cognitive_complexity)]
    fn test_round_with_precision() {
        let rm = RoundMethod::FromZero;
        assert_eq!(1.0, round_with_precision(0.12345, rm, 0));
        assert_eq!(0.2, round_with_precision(0.12345, rm, 1));
        assert_eq!(0.13, round_with_precision(0.12345, rm, 2));
        assert_eq!(0.124, round_with_precision(0.12345, rm, 3));
        assert_eq!(0.1235, round_with_precision(0.12345, rm, 4));
        assert_eq!(0.12345, round_with_precision(0.12345, rm, 5));

        let rm = RoundMethod::TowardsZero;
        assert_eq!(0.0, round_with_precision(0.12345, rm, 0));
        assert_eq!(0.1, round_with_precision(0.12345, rm, 1));
        assert_eq!(0.12, round_with_precision(0.12345, rm, 2));
        assert_eq!(0.123, round_with_precision(0.12345, rm, 3));
        assert_eq!(0.1234, round_with_precision(0.12345, rm, 4));
        assert_eq!(0.12345, round_with_precision(0.12345, rm, 5));
    }

    #[test]
    fn test_parse_implicit_precision() {
        assert_eq!(0, parse_implicit_precision(""));
        assert_eq!(0, parse_implicit_precision("1"));
        assert_eq!(1, parse_implicit_precision("1.2"));
        assert_eq!(2, parse_implicit_precision("1.23"));
        assert_eq!(3, parse_implicit_precision("1.234"));
        assert_eq!(0, parse_implicit_precision("1K"));
        assert_eq!(1, parse_implicit_precision("1.2K"));
        assert_eq!(2, parse_implicit_precision("1.23K"));
        assert_eq!(3, parse_implicit_precision("1.234K"));
    }

    #[test]
    fn parse_suffix_accepts_additional_nbsp_variants() {
        for ch in ['\u{00A0}', '\u{2007}', '\u{202F}', '\u{2003}'] {
            let input = format!("2{ch}K");
            let numeric_locale = locale::get_numeric_locale();
            let (value, suffix) = parse_suffix(&input, None, numeric_locale).unwrap();
            assert_eq!(value, 2.0);
            assert_eq!(suffix, Some((RawSuffix::K, false)));
        }
    }

    #[test]
    fn parse_suffix_accepts_word_joiner() {
        let input = "2\u{2060}Ki";
        let numeric_locale = locale::get_numeric_locale();
        let (value, suffix) = parse_suffix(input, None, numeric_locale).unwrap();
        assert_eq!(value, 2.0);
        assert_eq!(suffix, Some((RawSuffix::K, true)));
    }
}
