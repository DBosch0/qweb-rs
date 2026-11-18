//! Number formatting identical to Python's `str()` / `repr()`.
//!
//! Hosts use it to write numbers straight into the output, without creating
//! an intermediate string value.

use std::fmt::{self, Write};

/// A string formatted in a fixed-size stack buffer (no allocation).
struct StackStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackStr<N> {
    fn new() -> Self {
        StackStr {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        // Only whole `&str`s are copied in.
        str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for StackStr<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        if end > N {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Python `repr(float)` appended to `out`: the shortest digits that round-trip,
/// laid out like `float_repr_style == 'short'` (scientific notation below
/// `1e-4` and from `1e16`).
///
/// ```
/// let mut s = String::new();
/// qweb_core::num::write_float_repr(1e16, &mut s);
/// qweb_core::num::write_float_repr(-0.5, &mut s);
/// assert_eq!(s, "1e+16-0.5");
/// ```
pub fn write_float_repr(f: f64, out: &mut String) {
    if f.is_nan() {
        return out.push_str("nan");
    }
    if f.is_infinite() {
        return out.push_str(if f > 0.0 { "inf" } else { "-inf" });
    }
    if f == 0.0 {
        return out.push_str(if f.is_sign_negative() { "-0.0" } else { "0.0" });
    }
    if write_short_decimal(f, out) {
        return;
    }
    // Shortest round-trip digits and exponent, then Python's layout rules.
    // Both are formatted on the stack (at most 17 digits plus the exponent).
    let mut e = StackStr::<32>::new();
    let _ = write!(e, "{:e}", f.abs());
    // `{:e}` always writes `<digits>e<exponent>`.
    let (mant, exp) = e.as_str().split_once('e').unwrap_or((e.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits = StackStr::<32>::new();
    for part in mant.split('.') {
        let _ = digits.write_str(part);
    }
    let mut even = StackStr::<32>::new();
    let digits = match half_even(f.abs(), digits.as_str(), exp) {
        Some(m) => {
            let _ = write!(even, "{m}");
            even.as_str()
        }
        None => digits.as_str(),
    };
    let decpt = exp + 1;
    if f < 0.0 {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(digits);
    } else {
        let d = decpt as usize;
        if digits.len() <= d {
            out.push_str(digits);
            for _ in digits.len()..d {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..d]);
            out.push('.');
            out.push_str(&digits[d..]);
        }
    }
}

/// Python's choice when `a` (positive, finite) is exactly halfway between two
/// shortest decimals that both round-trip: the one with an even last digit
/// (David Gay's `dtoa` rounds half to even, Rust picks either). `digits` and
/// `exp` are Rust's shortest digits and their scientific exponent. Returns
/// the digits to use instead, or `None` when `digits` are right already.
fn half_even(a: f64, digits: &str, exp: i32) -> Option<u64> {
    // a == odd * 2^two, exactly
    let bits = a.to_bits();
    let biased = (bits >> 52) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (mant, two) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased - 1075)
    };
    let zeros = mant.trailing_zeros();
    let (odd, two) = (u128::from(mant >> zeros), two + zeros as i32);
    // The digits are the integer `m`, and a == m * 10^e within half a unit.
    // The midpoints around `m` are (2m ± 1) * 5^e * 2^(e - 1): an odd number
    // times a power of two, like `a`.
    let e = exp - (digits.len() as i32 - 1);
    if two != e - 1 || e.abs() > 27 {
        // (beyond 5^27, one side is too large to equal the other)
        return None;
    }
    let m: u64 = digits.parse().ok()?;
    let pow = 5u128.pow(e.unsigned_abs());
    let is_midpoint = |twice: u64| {
        if e >= 0 {
            u128::from(twice).checked_mul(pow) == Some(odd)
        } else {
            odd.checked_mul(pow) == Some(u128::from(twice))
        }
    };
    if m.is_multiple_of(2) {
        None
    } else if is_midpoint(2 * m - 1) {
        Some(m - 1)
    } else if is_midpoint(2 * m + 1) {
        Some(m + 1)
    } else {
        None
    }
}

/// Fast path of [`write_float_repr`] for decimals with few digits (prices,
/// quantities, ...): `f == n / 10^k` with `|n| < 2^50` and `k <= 6`, at
/// least `1e-4` in magnitude. Returns `false` (nothing written) otherwise.
///
/// For the smallest such `k`, `n` is the shortest digit string that
/// round-trips: decimals with `k` fractional digits are at least 4 ulps of
/// `f` apart (`n < 2^50`), so `n / 10^k` is the only one that reads back as
/// `f`, and no decimal with fewer digits does (the nearest one was tried).
/// `n / 10^k` is exact arithmetic on two integers below `2^53` followed by
/// one correctly rounded division, which is how the decimal is read.
#[allow(
    clippy::float_cmp,
    reason = "the decimal must read back as exactly the same float"
)]
fn write_short_decimal(f: f64, out: &mut String) -> bool {
    const LIMIT: f64 = (1u64 << 50) as f64;
    let a = f.abs();
    if !(1e-4..LIMIT).contains(&a) {
        return false;
    }
    let mut scale = 1.0;
    let mut pow = 1u64;
    for k in 0..=6 {
        let scaled = a * scale;
        if scaled >= LIMIT {
            return false;
        }
        // Nearest integer: `scaled + 0.5` is exact below 2^52.
        let n = (scaled + 0.5) as u64;
        if n as f64 / scale == a {
            if f < 0.0 {
                out.push('-');
            }
            write_int((n / pow) as i64, out);
            let mut frac = [b'0'; 6];
            let mut rest = n % pow;
            for digit in frac[..k].iter_mut().rev() {
                *digit = b'0' + (rest % 10) as u8;
                rest /= 10;
            }
            out.push('.');
            // ASCII digits only.
            out.push_str(str::from_utf8(&frac[..k.max(1)]).unwrap_or(""));
            return true;
        }
        scale *= 10.0;
        pow *= 10;
    }
    false
}

/// Python `str(int)` appended to `out`.
///
/// ```
/// let mut s = String::new();
/// qweb_core::num::write_int(-42, &mut s);
/// assert_eq!(s, "-42");
/// ```
pub fn write_int(i: i64, out: &mut String) {
    // 20 bytes hold `u64::MAX`; the digits are written from the end.
    let mut buf = [0u8; 20];
    let mut pos = buf.len();
    let mut n = i.unsigned_abs();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if i < 0 {
        out.push('-');
    }
    // ASCII digits only.
    out.push_str(str::from_utf8(&buf[pos..]).unwrap_or(""));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn float(f: f64) -> String {
        let mut s = String::new();
        write_float_repr(f, &mut s);
        s
    }

    #[test]
    #[allow(
        clippy::excessive_precision,
        reason = "the halfway cases are exact floats; clippy rounds them like Rust prints them"
    )]
    fn floats() {
        for (f, repr) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.25e-7, "1.25e-07"),
            (-748.5, "-748.5"),
            (1e100, "1e+100"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (0.1 + 0.2, "0.30000000000000004"),
            // halfway between two shortest decimals: the even one
            (938_072_170_685_064.25, "938072170685064.2"),
            (9_725_986_424_039.062_5, "9725986424039.062"),
            (88_588_272_501_732.625, "88588272501732.62"),
            (2_657_719_993_437.031_25, "2657719993437.0312"),
            (1_187_963_360_343_591.25, "1187963360343591.2"),
            (1_125_899_906_842_623.75, "1125899906842623.8"),
        ] {
            assert_eq!(float(f), repr);
        }
        assert_eq!(float(f64::NAN), "nan");
    }

    /// The fast path agrees with the general one (shortest digits of `{:e}`).
    #[test]
    fn short_decimals() {
        // xorshift: deterministic pseudo-random values
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut fast = 0;
        for i in 0..400_000 {
            let r = next();
            let f = match i % 4 {
                // decimals: n / 10^k
                0 => (r % 10_000_000) as f64 / 10f64.powi((next() % 8) as i32),
                1 => (r % (1 << 52)) as f64 / 10f64.powi((next() % 9) as i32),
                // sums of decimals (not the nearest double of a short decimal)
                2 => (r % 1000) as f64 * 0.1 + (next() % 1000) as f64 * 0.01,
                // any bit pattern
                _ => f64::from_bits(r),
            };
            for f in [f, -f] {
                if f.is_nan() {
                    continue;
                }
                let mut short = String::new();
                if write_short_decimal(f, &mut short) {
                    fast += 1;
                    // Python reads the text back as the same float and no
                    // shorter text does: compare with Rust's shortest repr.
                    assert_eq!(short.parse::<f64>().ok(), Some(f), "{short}");
                    let digits = |s: &str| {
                        s.trim_start_matches(['-', '0', '.'])
                            .trim_end_matches(".0")
                            .replace('.', "")
                    };
                    let std = format!("{:e}", f.abs());
                    let std = std.split('e').next().unwrap_or("");
                    assert_eq!(
                        digits(&short).trim_end_matches('0'),
                        digits(std).trim_end_matches('0'),
                        "{f:?}"
                    );
                }
            }
        }
        assert!(fast > 100_000, "{fast}");
    }

    #[test]
    fn ints() {
        for i in [0, 7, -7, 10, 1_234_567_890, i64::MAX, i64::MIN] {
            let mut s = String::new();
            write_int(i, &mut s);
            assert_eq!(s, i.to_string());
        }
    }
}
