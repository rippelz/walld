//! Wallpaper Engine GLSL preprocess → GLSL ES 3.00.
//!
//! Ports the essential transforms from WE / linux-wallpaperengine:
//! HLSL-ish helpers, attribute/varying → in/out, includes, combos.

use crate::assets::AssetResolver;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Fragment,
}

/// Preprocess a WE shader source into GLES 3.00 text ready to compile.
pub fn preprocess_we_glsl(
    source: &str,
    stage: ShaderStage,
    filename: &str,
    assets: &AssetResolver,
    combos: &HashMap<String, i32>,
) -> String {
    let mut body = source.to_string();

    // Expand #include **in place** (WE order). Bagging includes and injecting
    // at the first `#include` marker put helper functions *before* the
    // `uniform sampler2D g_Texture0` they reference (shine_gaussian /
    // common_blur.h → undeclared g_Texture0). Expanding in place keeps
    // uniforms declared above any included functions that use them, while
    // still letting pre-main helpers (lensflare → common.h) see includes.
    // Includes land before int/float rewrites so common_*.h get the same fixes.
    body = expand_includes_inplace(&body, assets, 0);

    // Combos as #defines
    let mut combo_defs = String::new();
    for (k, v) in combos {
        combo_defs.push_str(&format!("#define {k} {v}\n"));
    }
    // WE predefines every declared combo (unset = 0). GLES rejects undefined
    // macros inside `#if` expressions, so any identifier referenced there but
    // never defined must default to 0 — otherwise the shader fails to compile.
    for name in undefined_conditional_idents(&body, combos) {
        combo_defs.push_str(&format!("#define {name} 0\n"));
    }

    // Common WE → GLSL macros. CAST* force float so CAST2(3) is legal in ES.
    let header = format!(
        r#"#version 300 es
// processed {filename}
precision highp float;
precision highp int;
#define mul(x, y) ((y) * (x))
#define lerp mix
#define frac fract
#define CAST2(x) (vec2(float(x)))
#define CAST3(x) (vec3(float(x)))
#define CAST4(x) (vec4(float(x)))
#define CAST3X3(x) (mat3(x))
#define float2 vec2
#define float3 vec3
#define float4 vec4
#define int2 ivec2
#define int3 ivec3
#define int4 ivec4
#define saturate(x) (clamp(x, 0.0, 1.0))
#define texSample2D texture
#define texSample2DLod textureLod
#define atan2 atan
#define fmod(x, y) ((x)-(y)*trunc((x)/(y)))
#define ddx dFdx
#define ddy(x) dFdy(-(x))
#define GLSL 1
// NOTE: GLSL ES 3.00 forbids overloading built-ins, so loosely-typed workshop
// shaders (int/float mixing) may fail to compile; those passes are skipped and
// the layer draws unmodified rather than disappearing.
{combo_defs}
"#
    );

    let stage_defs = match stage {
        ShaderStage::Vertex => "#define attribute in\n#define varying out\n",
        ShaderStage::Fragment => {
            "out vec4 out_FragColor;\n#define varying in\n#define gl_FragColor out_FragColor\n"
        }
    };

    // Replace gl_FragColor if still present as token after define
    body = body.replace("gl_FragColor", "out_FragColor");

    // attribute → in, varying → out/in already via defines; also bare replacements
    // for lines that don't use the macro form cleanly
    if stage == ShaderStage::Vertex {
        body = replace_keyword_decl(&body, "attribute", "in");
        body = replace_keyword_decl(&body, "varying", "out");
    } else {
        body = replace_keyword_decl(&body, "varying", "in");
    }

    // HLSL float suffix (`1.0f`) is not valid GLSL.
    body = strip_hlsl_float_suffixes(&body);

    // GLES ES forbids int/float mixing in arithmetic and mix(). Workshop
    // shaders freely write `pointer * 2 - 1` and `mix(999, …)`.
    body = promote_int_literals_in_mix(&body);
    body = promote_common_int_float_ops(&body);
    // `sample` is a reserved word in GLSL ES 3.00 (texture sampling).
    // WE HLSL-style shaders use it as a local: `vec4 sample = texSample2D(...)`.
    body = rename_reserved_identifiers(&body);
    // Promote loose integer literals in float contexts; keep pure-int loops/decls.
    body = promote_loose_int_literals(&body);
    // `const float x = sampleCount - 1` (int expr) → wrap with float(...).
    body = fix_float_inits_from_int_expr(&body);
    // `i / sampleDrop` where i is int loop var.
    body = body
        .replace("(i / ", "(float(i) / ")
        .replace("(i/", "(float(i)/");

    // Pin attributes *after* int→float rewrites so we emit `location=0` not
    // `location=0.0` (layout indices must be integer constants in GLES 3.00).
    if stage == ShaderStage::Vertex {
        body = pin_attribute_locations(&body);
    }

    // WE often `#include "common_blur.h"` *before* `uniform sampler2D g_Texture0`.
    // GLES requires identifiers to be declared before use in function bodies,
    // so hoist all global decls (uniform/in/out/…) above any functions.
    body = hoist_global_decls(&body);

    format!("{header}{stage_defs}\n{body}")
}

/// Hoist unconditional interface declarations without changing lexical or
/// preprocessor scope. Constants stay in place: they may depend on local
/// variables or macros and are not shader interface declarations.
fn hoist_global_decls(src: &str) -> String {
    let mut decls: Vec<&str> = Vec::new();
    let mut rest: Vec<&str> = Vec::new();
    let mut brace_depth = 0i32;
    let mut conditional_depth = 0usize;
    let mut in_comment = false;
    for line in src.lines() {
        // Count only code braces/directives. JSON annotations and block
        // comments frequently contain braces which are not shader scopes.
        let mut code = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_comment {
                if c == '*' && chars.peek() == Some(&'/') { chars.next(); in_comment = false; }
            } else if c == '/' && chars.peek() == Some(&'/') {
                break;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next(); in_comment = true;
            } else { code.push(c); }
        }
        let directive = code.trim_start().strip_prefix('#')
            .and_then(|s| s.split_whitespace().next()).unwrap_or("");
        if matches!(directive, "if" | "ifdef" | "ifndef") { conditional_depth += 1; }
        if directive == "endif" { conditional_depth = conditional_depth.saturating_sub(1); }
        if brace_depth == 0 && conditional_depth == 0 && is_global_decl_line(&code) {
            decls.push(line);
        } else {
            rest.push(line);
        }
        if !code.trim_start().starts_with('#') {
            brace_depth += code.chars().filter(|c| *c == '{').count() as i32;
            brace_depth -= code.chars().filter(|c| *c == '}').count() as i32;
        }
    }
    if decls.is_empty() {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len() + 16);
    out.push_str("// --- hoisted globals ---\n");
    for d in &decls {
        out.push_str(d);
        out.push('\n');
    }
    out.push_str("// --- end hoisted ---\n");
    for r in &rest {
        out.push_str(r);
        out.push('\n');
    }
    out
}

fn is_global_decl_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with("//") || t.starts_with('#') {
        return false;
    }
    // Strip trailing `// …` so JSON annotations like `// {"hidden":true}`
    // don't trip the `{` check (that was leaving `uniform sampler2D g_Texture0`
    // below included blur helpers → undeclared identifier).
    let code = t.split("//").next().unwrap_or(t).trim_end();
    // layout(location=…) in/out …
    let code = if code.starts_with("layout(") {
        code.find(')')
            .map(|i| code[i + 1..].trim_start())
            .unwrap_or(code)
    } else {
        code
    };
    if code.starts_with("uniform ")
        || code.starts_with("in ")
        || code.starts_with("out ")
        || code.starts_with("attribute ")
        || code.starts_with("varying ")
    {
        // Not a function: must end with `;` on this line.
        return code.contains(';') && !code.contains('{');
    }
    false
}

fn replace_keyword_decl(src: &str, from: &str, to: &str) -> String {
    // Replace "attribute " / "varying " at line starts
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with(from) && t[from.len()..].starts_with(char::is_whitespace) {
            let indent_len = line.len() - t.len();
            out.push_str(&line[..indent_len]);
            out.push_str(to);
            out.push_str(&t[from.len()..]);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `mix(999, 1.0 / x, t)` → `mix(999.0, …)` so GLES accepts the call.
fn promote_int_literals_in_mix(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        if i + 4 <= bytes.len() && &bytes[i..i + 4] == b"mix(" {
            out.push_str("mix(");
            i += 4;
            // Scan arguments until matching ')' at depth 0, rewriting bare ints.
            let mut depth = 1i32;
            let mut arg_start = i;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'(' => {
                        depth += 1;
                        i += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            // final arg
                            let arg = std::str::from_utf8(&bytes[arg_start..i]).unwrap_or("");
                            out.push_str(&promote_bare_int_literal(arg));
                            out.push(')');
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    b',' if depth == 1 => {
                        let arg = std::str::from_utf8(&bytes[arg_start..i]).unwrap_or("");
                        out.push_str(&promote_bare_int_literal(arg));
                        out.push(',');
                        i += 1;
                        arg_start = i;
                    }
                    _ => i += 1,
                }
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn promote_bare_int_literal(arg: &str) -> String {
    let t = arg.trim();
    if t.is_empty() {
        return arg.to_string();
    }
    // Pure integer literal (optional leading sign), not already a float.
    let body = t.strip_prefix('+').or_else(|| t.strip_prefix('-')).unwrap_or(t);
    if !body.is_empty()
        && body.bytes().all(|b| b.is_ascii_digit())
        && !t.contains('.')
        && !t.ends_with('u')
        && !t.ends_with('U')
    {
        return format!("{t}.0");
    }
    arg.to_string()
}

/// Rewrite frequent WE NDC / scale patterns that mix int and float.
///
/// Deliberately does **not** rewrite bare `= 0;` / `= 1;` — those appear in
/// `for (int i = 0; …)` and turning them into `0.0` breaks the loop.
fn promote_common_int_float_ops(src: &str) -> String {
    let mut s = src.to_string();
    // `pointer * 2 - 1` → float ops (xray.vert and many others).
    for (from, to) in [
        ("* 2 - 1", "* 2.0 - 1.0"),
        ("*2 - 1", "*2.0 - 1.0"),
        ("* 2-1", "* 2.0-1.0"),
        ("*2-1", "*2.0-1.0"),
        ("* 2 + 1", "* 2.0 + 1.0"),
        ("* 2.0 - 1,", "* 2.0 - 1.0,"),
        ("* 2.0 - 1)", "* 2.0 - 1.0)"),
        ("* 2,", "* 2.0,"),
        ("* 2)", "* 2.0)"),
        ("* 2 ", "* 2.0 "),
        ("+ 2)", "+ 2.0)"),
        ("- 2)", "- 2.0)"),
        ("/ 2)", "/ 2.0)"),
        ("/ 2;", "/ 2.0;"),
        ("/ 2,", "/ 2.0,"),
        ("/ 2 ", "/ 2.0 "),
    ] {
        s = s.replace(from, to);
    }
    s
}

/// Strip HLSL-style float suffixes (`1.0f`, `.5F`) which GLSL rejects.
fn strip_hlsl_float_suffixes(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        // Match a float literal ending in f/F (not part of an identifier).
        if bytes[i].is_ascii_digit() || (bytes[i] == b'.' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_digit())) {
            let start = i;
            // integer part
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let mut is_float = false;
            if i < bytes.len() && bytes[i] == b'.' {
                is_float = true;
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // exponent
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                is_float = true;
                i += 1;
                if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let lit = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            out.push_str(lit);
            // Drop trailing f/F only when this was (or becomes) a float literal.
            if i < bytes.len() && (bytes[i] == b'f' || bytes[i] == b'F') {
                let next = bytes.get(i + 1).copied().unwrap_or(0);
                // Don't strip if it's an identifier like `1foo`.
                if !is_ident_cont(next) && (is_float || lit.contains('.')) {
                    i += 1; // skip f/F
                    continue;
                }
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Promote bare integer literals to floats in non-int contexts.
///
/// Walks line-by-line: skips preprocessor lines, promotes ints, then demotes
/// back to integers inside pure-int declarations and for-loop headers so
/// `for (int i = 0; i < sampleCount; ++i)` stays valid.
fn promote_loose_int_literals(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Never promote inside layout(...) — `location=0.0` is illegal.
        if t.starts_with("layout(") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let promoted = promote_bare_ints_in_expr(line);
        let fixed = demote_ints_in_int_context(&promoted);
        out.push_str(&fixed);
        out.push('\n');
    }
    out
}

/// After promoting bare ints, restore integer literals in int-typed contexts.
fn demote_ints_in_int_context(line: &str) -> String {
    let t = line.trim_start();
    // for (int i = 0.0; …) / const int x = 30.0; / int n = 4.0;
    let is_int_ctx = t.contains("int ")
        || t.contains("int\t")
        || t.starts_with("int ")
        || t.contains("for (int")
        || t.contains("for(int");
    if is_int_ctx {
        // Demote N.0 → N only when it's a whole-number float literal.
        return demote_whole_float_literals(line);
    }
    // Array sizes only: `foo[30.0]` → `foo[30]`, but leave `m[0][2] = 0.0`
    // alone (matrix elements are floats — demoting the RHS breaks GLES).
    if line.contains('[') {
        return demote_array_dimension_floats(line);
    }
    line.to_string()
}

/// Demote `name[N.0]` → `name[N]` without touching float literals outside `[]`.
fn demote_array_dimension_floats(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut bracket_depth = 0i32;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => {
                bracket_depth += 1;
                out.push('[');
                i += 1;
            }
            b']' => {
                bracket_depth = (bracket_depth - 1).max(0);
                out.push(']');
                i += 1;
            }
            b if b.is_ascii_digit() && bracket_depth > 0 => {
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b'0' {
                    let after = bytes.get(i + 2).copied().unwrap_or(0);
                    if !after.is_ascii_digit() && after != b'e' && after != b'E' {
                        out.push_str(std::str::from_utf8(&bytes[start..i]).unwrap_or(""));
                        i += 2;
                        continue;
                    }
                }
                out.push_str(std::str::from_utf8(&bytes[start..i]).unwrap_or(""));
            }
            _ => {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    out
}

fn demote_whole_float_literals(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // Exactly `digits.0` (not `digits.5` or `digits.0e3`)
            if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1] == b'0' {
                let after = bytes.get(i + 2).copied().unwrap_or(0);
                if !after.is_ascii_digit() && after != b'e' && after != b'E' {
                    let num = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                    out.push_str(num);
                    i += 2; // skip .0
                    continue;
                }
            }
            out.push_str(std::str::from_utf8(&bytes[start..i]).unwrap_or(""));
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Fix `const float sampleDrop = sampleCount - 1` style inits where the RHS is
/// an int expression (or int ident ± float lit after bare-int promotion).
///
/// Deliberately only touches `const float` lines — applying this to every
/// `float x = …` local rewrote swizzles (`Q.x` → `float(Q).float(x)`) and
/// scientific notation (`1e-10` → `1float(e)-10`).
fn fix_float_inits_from_int_expr(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 32);
    for line in src.lines() {
        let t = line.trim_start();
        if !t.starts_with("const float ") || !t.contains('=') {
            // Workshop: `float pointer = g_PointerPosition.xy * k` (vec→scalar).
            if t.starts_with("float ")
                && !t.starts_with("float2")
                && !t.starts_with("float3")
                && !t.starts_with("float4")
                && t.contains('=')
            {
                if let Some(fixed) = fix_float_from_vector_rhs(line) {
                    out.push_str(&fixed);
                    out.push('\n');
                    continue;
                }
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let Some(eq) = line.find('=') else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let (lhs, rhs_with_eq) = line.split_at(eq);
        let rhs = &rhs_with_eq[1..];
        let (rhs_body, semi) = match rhs.rfind(';') {
            Some(i) => (&rhs[..i], &rhs[i..]),
            None => (rhs, ""),
        };
        let trimmed = rhs_body.trim();
        if trimmed.starts_with("float(") {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Pure int expression (idents + ops + ints, no dots, no calls).
        if is_pure_int_expr(trimmed) {
            out.push_str(lhs);
            out.push_str("= float(");
            out.push_str(trimmed);
            out.push(')');
            out.push_str(semi);
            out.push('\n');
            continue;
        }
        // `sampleCount - 1.0` / `1.0 * sampleCount` after bare-int promotion:
        // cast multi-char idents that aren't after `.` and aren't calls.
        let casted = cast_const_int_idents(trimmed);
        out.push_str(lhs);
        out.push('=');
        if rhs_body.starts_with(' ') || rhs_body.starts_with('\t') {
            let ws: String = rhs_body.chars().take_while(|c| c.is_whitespace()).collect();
            out.push_str(&ws);
        } else {
            out.push(' ');
        }
        out.push_str(&casted);
        out.push_str(semi);
        out.push('\n');
    }
    out
}

fn is_pure_int_expr(expr: &str) -> bool {
    if expr.is_empty() || expr.contains('.') || expr.contains('(') || expr.contains('[') {
        return false;
    }
    // Only idents, digits, whitespace, and + - * / ( ).
    expr.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c.is_whitespace() || matches!(c, '+' | '-' | '*' | '/' | '(' | ')'))
}

/// Cast multi-char identifiers in a const-float RHS (e.g. sampleCount), never
/// single-letter swizzles/loop vars, never after `.`, never function names.
fn cast_const_int_idents(expr: &str) -> String {
    const SKIP: &[&str] = &[
        "float", "vec2", "vec3", "vec4", "int", "true", "false", "sin", "cos", "tan", "pow",
        "abs", "min", "max", "mix", "clamp", "dot", "mod", "floor", "ceil", "fract", "frac",
        "length", "normalize", "step", "smoothstep", "texture", "texSample2D", "saturate",
    ];
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            // If previous non-space char is `.`, this is a swizzle/field — copy as-is.
            let mut prev = start;
            while prev > 0 && bytes[prev - 1].is_ascii_whitespace() {
                prev -= 1;
            }
            let after_dot = prev > 0 && bytes[prev - 1] == b'.';
            i += 1;
            while i < bytes.len() && is_ident_cont(bytes[i]) {
                i += 1;
            }
            let ident = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            let is_call = expr[i..].trim_start().starts_with('(');
            // Multi-char only (skip x/y/z/w/i/e); skip uniforms and known funcs.
            let should = !after_dot
                && !is_call
                && ident.len() > 1
                && !SKIP.contains(&ident)
                && !ident.starts_with("g_")
                && !ident.starts_with("u_")
                && !ident.starts_with("v_")
                && !ident.starts_with("a_");
            if should {
                out.push_str("float(");
                out.push_str(ident);
                out.push(')');
            } else {
                out.push_str(ident);
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// `float pointer = g_PointerPosition.xy * u_pointerSpeed` → take `.x`.
///
/// Only rewrites simple expressions (no function calls). Applying this to
/// `max(0.0, dot(n.xy, d))` would append a bogus `.x` on a float.
fn fix_float_from_vector_rhs(line: &str) -> Option<String> {
    let eq = line.find('=')?;
    let (lhs, rhs_with_eq) = line.split_at(eq);
    let rhs = &rhs_with_eq[1..];
    let (rhs_body, semi) = match rhs.rfind(';') {
        Some(i) => (&rhs[..i], &rhs[i..]),
        None => (rhs, ""),
    };
    let trimmed = rhs_body.trim();
    if trimmed.contains('(') {
        return None;
    }
    if !has_vector_swizzle(trimmed) || has_scalar_swizzle_end(trimmed) {
        return None;
    }
    // Avoid double-wrapping.
    if trimmed.starts_with('(') && trimmed.contains(").x") {
        return None;
    }
    Some(format!("{lhs}= ({trimmed}).x{semi}"))
}

/// Byte index of the first `// #include` marker left by expand_includes.
fn find_first_include_marker(src: &str) -> Option<usize> {
    src.find("// #include").or_else(|| src.find("//#include"))
}

/// `in vec3 a_Position` → `layout(location = 0) in vec3 a_Position` (and UV → 1).
fn pin_attribute_locations(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 64);
    for line in src.lines() {
        let t = line.trim_start();
        let indent_len = line.len() - t.len();
        let pinned = if t.starts_with("layout(") {
            // already has layout
            None
        } else if t.starts_with("in ") && t.contains("a_Position") {
            Some(format!(
                "{}layout(location = 0) {}",
                &line[..indent_len],
                t
            ))
        } else if t.starts_with("in ") && (t.contains("a_TexCoord") || t.contains("a_TexCoords"))
        {
            Some(format!(
                "{}layout(location = 1) {}",
                &line[..indent_len],
                t
            ))
        } else {
            None
        };
        if let Some(p) = pinned {
            out.push_str(&p);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn has_vector_swizzle(expr: &str) -> bool {
    let b = expr.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'.' {
            continue;
        }
        let mut j = i + 1;
        while j < b.len() && matches!(b[j], b'x' | b'y' | b'z' | b'w' | b'r' | b'g' | b'b' | b'a') {
            j += 1;
        }
        if j - i - 1 >= 2 {
            return true;
        }
    }
    false
}

fn has_scalar_swizzle_end(expr: &str) -> bool {
    let t = expr.trim_end();
    t.ends_with(".x")
        || t.ends_with(".y")
        || t.ends_with(".z")
        || t.ends_with(".w")
        || t.ends_with(".r")
        || t.ends_with(".g")
        || t.ends_with(".b")
        || t.ends_with(".a")
}

/// Rename GLSL ES reserved identifiers used as variables in WE shaders.
fn rename_reserved_identifiers(src: &str) -> String {
    // Word-boundary replace of `sample` → `we_sample` (not texSample2D).
    let mut out = String::with_capacity(src.len() + 32);
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_cont(bytes[i]) {
                i += 1;
            }
            let ident = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            // Don't rewrite if this is part of texSample2D / textureSample etc.
            let prev_is_alnum = start > 0 && is_ident_cont(bytes[start - 1]);
            if !prev_is_alnum && ident == "sample" {
                out.push_str("we_sample");
            } else if !prev_is_alnum && ident == "input" {
                // less common but also reserved in some profiles
                out.push_str("we_input");
            } else {
                out.push_str(ident);
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn promote_bare_ints_in_expr(expr: &str) -> String {
    let bytes = expr.as_bytes();
    let mut out = String::with_capacity(expr.len() + 8);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            // Skip if part of a larger token (1e-3, 0x, 30.0, ident123).
            // Also skip the *exponent* digits of scientific notation: after
            // `1e-` the `10` in `1e-10` must stay an integer or we emit `1e-10.0`.
            let prev = if start == 0 {
                0
            } else {
                bytes[start - 1]
            };
            let prev2 = if start < 2 { 0 } else { bytes[start - 2] };
            let in_exponent = matches!(prev, b'e' | b'E')
                || (matches!(prev, b'+' | b'-') && matches!(prev2, b'e' | b'E'));
            let prev_ok = start == 0
                || (!is_ident_cont(prev) && prev != b'.' && !in_exponent);
            let next = bytes.get(i).copied().unwrap_or(0);
            let next_ok = next != b'.'
                && next != b'e'
                && next != b'E'
                && next != b'x'
                && next != b'X'
                && !is_ident_cont(next);
            let num = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            if prev_ok && next_ok && !num.is_empty() {
                out.push_str(num);
                out.push_str(".0");
            } else {
                out.push_str(num);
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Find the entry-point `void main(...)` only — never a local like `vec2 main = …`
/// (workshop lens-flare shaders declare that identifier).
fn find_main_function(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if &bytes[i..i + 4] == b"main" {
            let before_ok = i == 0 || !is_ident_cont(bytes[i - 1]);
            let after = bytes.get(i + 4).copied().unwrap_or(b' ');
            if before_ok && (after == b'(' || after == b' ' || after == b'\t' || after == b'\r') {
                // Look back on the same line for `void`.
                let line_start = src[..i].rfind('\n').map(|x| x + 1).unwrap_or(0);
                let prefix = src[line_start..i].trim();
                // `void main` or `void  main` (also tolerate qualifiers).
                if prefix == "void" || prefix.ends_with(" void") || prefix.ends_with("\tvoid") {
                    return Some(line_start);
                }
            }
        }
        i += 1;
    }
    None
}

/// Expand `#include "foo.h"` in place (recursively).
fn expand_includes_inplace(src: &str, assets: &AssetResolver, depth: u32) -> String {
    if depth > 8 {
        return src.to_string();
    }
    let mut out = String::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("#include") {
            if let Some(q0) = t.find('"') {
                if let Some(q1) = t[q0 + 1..].find('"') {
                    let name = &t[q0 + 1..q0 + 1 + q1];
                    out.push_str(&format!("// #include \"{name}\"\n"));
                    match assets.include_shader(name) {
                        Ok(content) => {
                            let nested = expand_includes_inplace(&content, assets, depth + 1);
                            out.push_str(&format!(
                                "// begin include {name}\n{nested}// end include {name}\n"
                            ));
                        }
                        Err(_) => {
                            out.push_str(&format!("// missing include {name}\n"));
                        }
                    }
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// After independent vert/frag preprocess, align matching `in`/`out` varyings
/// that disagree on type (workshop rotate2d: vert `vec2 v_TexCoord` vs frag
/// `vec3 v_TexCoord` → link failure). Widens the narrower side.
pub fn reconcile_stage_varyings(vert: &mut String, frag: &mut String) {
    let v_outs = collect_io_decls(vert, "out");
    let f_ins = collect_io_decls(frag, "in");
    for (name, fty) in &f_ins {
        let Some(vty) = v_outs.get(name) else {
            continue;
        };
        if vty == fty {
            continue;
        }
        // Prefer the fragment's type (usually the wider one: vec3 vs vec2).
        if type_rank(fty) > type_rank(vty) {
            *vert = rewrite_io_type(vert, "out", name, fty);
            // If the vert only assigns .xy, zero-fill remaining components.
            *vert = pad_varying_assignment(vert, name, fty);
        } else if type_rank(vty) > type_rank(fty) {
            *frag = rewrite_io_type(frag, "in", name, vty);
        }
    }
}

fn type_rank(ty: &str) -> u8 {
    match ty {
        "float" => 1,
        "vec2" => 2,
        "vec3" => 3,
        "vec4" => 4,
        _ => 0,
    }
}

fn collect_io_decls(src: &str, qual: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let prefix = format!("{qual} ");
    for line in src.lines() {
        let t = line.trim_start();
        // `out vec2 v_TexCoord;` or `layout(...) out vec2 v_TexCoord;`
        let t = if let Some(rest) = t.strip_prefix("layout(") {
            rest.find(')')
                .map(|i| rest[i + 1..].trim_start())
                .unwrap_or(t)
        } else {
            t
        };
        if !t.starts_with(&prefix) {
            continue;
        }
        let rest = &t[prefix.len()..];
        let mut parts = rest.split_whitespace();
        let Some(ty) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        let name = name.trim_end_matches(';').to_string();
        if name.starts_with('v') || name.contains("TexCoord") || name.contains("Coord") {
            out.insert(name, ty.to_string());
        }
    }
    out
}

fn rewrite_io_type(src: &str, qual: &str, name: &str, new_ty: &str) -> String {
    let mut out = String::with_capacity(src.len() + 8);
    for line in src.lines() {
        let t = line.trim_start();
        let is_match = (t.contains(&format!("{qual} ")) || t.contains(&format!(") {qual} ")))
            && t.contains(name)
            && t.contains(';');
        if is_match {
            // Replace the type token before `name`.
            if let Some(nidx) = t.find(name) {
                let before = &t[..nidx];
                // last word in `before` is the type
                let mut words: Vec<&str> = before.split_whitespace().collect();
                if let Some(last) = words.last_mut() {
                    if matches!(*last, "float" | "vec2" | "vec3" | "vec4") {
                        *last = new_ty;
                        let indent = line.len() - t.len();
                        out.push_str(&line[..indent]);
                        out.push_str(&words.join(" "));
                        out.push(' ');
                        out.push_str(&t[nidx..]);
                        out.push('\n');
                        continue;
                    }
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn pad_varying_assignment(src: &str, name: &str, new_ty: &str) -> String {
    // `v_TexCoord.xy = a_TexCoord;` → also set z (and w) to useful defaults.
    let pad = match new_ty {
        "vec3" => format!("\n\t{name}.z = 1.0;"),
        "vec4" => format!("\n\t{name}.z = 1.0;\n\t{name}.w = 1.0;"),
        _ => return src.to_string(),
    };
    let needle = format!("{name}.xy =");
    if let Some(idx) = src.find(&needle) {
        if let Some(semi) = src[idx..].find(';') {
            let at = idx + semi + 1;
            let mut out = String::with_capacity(src.len() + pad.len());
            out.push_str(&src[..at]);
            out.push_str(&pad);
            out.push_str(&src[at..]);
            return out;
        }
    }
    // `v_TexCoord = a_TexCoord.xyxy` style already full — leave alone.
    src.to_string()
}

/// Built-in waterflow fragment (GLES 300) — used when WE asset shaders fail to load.
/// Identifiers used in `#if` / `#elif` expressions that the source never
/// defines and the combo map doesn't provide. `defined(X)` guards are skipped
/// (those are legal with undefined macros).
fn undefined_conditional_idents(
    src: &str,
    combos: &HashMap<String, i32>,
) -> std::collections::BTreeSet<String> {
    let mut defined: std::collections::BTreeSet<String> = combos.keys().cloned().collect();
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#define ") {
            if let Some(name) = rest.split_whitespace().next() {
                defined.insert(name.split('(').next().unwrap_or(name).to_string());
            }
        }
    }
    let mut used = std::collections::BTreeSet::new();
    for line in src.lines() {
        let t = line.trim();
        let expr = if let Some(r) = t.strip_prefix("#if ") {
            r
        } else if let Some(r) = t.strip_prefix("#elif ") {
            r
        } else {
            continue;
        };
        // Drop `defined(X)` / `defined X` guards.
        let mut cleaned = String::new();
        let mut rest = expr;
        while let Some(p) = rest.find("defined") {
            cleaned.push_str(&rest[..p]);
            let after = &rest[p + "defined".len()..];
            let after = after.trim_start();
            let after = if let Some(a) = after.strip_prefix('(') {
                a.find(')').map(|q| &a[q + 1..]).unwrap_or("")
            } else {
                let skip = after
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                &after[skip..]
            };
            rest = after;
        }
        cleaned.push_str(rest);

        let mut cur = String::new();
        for ch in cleaned.chars().chain(std::iter::once(' ')) {
            if ch.is_alphanumeric() || ch == '_' {
                cur.push(ch);
            } else {
                if !cur.is_empty()
                    && !cur.chars().next().unwrap().is_ascii_digit()
                    && !defined.contains(&cur)
                {
                    used.insert(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
        }
    }
    used
}

pub fn waterflow_vert_es() -> &'static str {
    r#"#version 300 es
precision highp float;
layout(location=0) in vec2 aPos;
layout(location=1) in vec2 aUV;
uniform mat4 uMVP;
uniform float g_Time;
uniform float g_FlowSpeed;
uniform float g_PhaseFeather;
uniform vec4 g_Texture1Resolution;
uniform vec4 g_FrameWindow; // xy = UV offset, zw = UV scale (spritesheet frame)
out vec4 v_TexCoord;
out vec4 v_Cycles;
out vec2 v_Blend;
void main() {
    gl_Position = uMVP * vec4(aPos, 0.0, 1.0);
    v_TexCoord.xy = aUV * g_FrameWindow.zw + g_FrameWindow.xy;
    float rx = g_Texture1Resolution.z / max(g_Texture1Resolution.x, 1.0);
    float ry = g_Texture1Resolution.w / max(g_Texture1Resolution.y, 1.0);
    v_TexCoord.zw = vec2(aUV.x * rx, aUV.y * ry);
    vec4 cycles = vec4(
        fract(g_Time * g_FlowSpeed),
        fract(g_Time * g_FlowSpeed + 0.5),
        fract(0.25 + g_Time * g_FlowSpeed),
        fract(0.25 + g_Time * g_FlowSpeed + 0.5)
    );
    float blend = 2.0 * abs(cycles.x - 0.5);
    float blend2 = 2.0 * abs(cycles.z - 0.5);
    vec2 sp = vec2(0.5 - g_PhaseFeather, 0.5 + g_PhaseFeather);
    blend = smoothstep(sp.x, sp.y, blend);
    blend2 = smoothstep(sp.x, sp.y, blend2);
    v_Cycles = cycles - vec4(0.5);
    v_Blend = vec2(blend, blend2);
}
"#
}

pub fn waterflow_frag_es() -> &'static str {
    r#"#version 300 es
precision highp float;
in vec4 v_TexCoord;
in vec4 v_Cycles;
in vec2 v_Blend;
uniform sampler2D g_Texture0;
uniform sampler2D g_Texture1;
uniform sampler2D g_Texture2;
uniform float g_FlowAmp;
uniform float g_FlowPhaseScale;
out vec4 fragColor;
void main() {
    float flowPhase = texture(g_Texture2, v_TexCoord.xy * g_FlowPhaseScale).r;
    vec2 flowColors = texture(g_Texture1, v_TexCoord.zw).rg;
    vec2 flowMask = (flowColors.rg - vec2(0.498, 0.498)) * 2.0;
    float flowAmount = length(flowMask);
    vec4 flowUVOffset = vec4(flowMask.xyxy * g_FlowAmp * 0.1) * v_Cycles.xxyy;
    vec4 flowUVOffset2 = vec4(flowMask.xyxy * g_FlowAmp * 0.1) * v_Cycles.zzww;
    vec4 albedo = texture(g_Texture0, v_TexCoord.xy);
    vec4 flowAlbedo = mix(
        texture(g_Texture0, v_TexCoord.xy + flowUVOffset.xy),
        texture(g_Texture0, v_TexCoord.xy + flowUVOffset.zw),
        v_Blend.x
    );
    vec4 flowAlbedo2 = mix(
        texture(g_Texture0, v_TexCoord.xy + flowUVOffset2.xy),
        texture(g_Texture0, v_TexCoord.xy + flowUVOffset2.zw),
        v_Blend.y
    );
    flowAlbedo = mix(flowAlbedo, flowAlbedo2, smoothstep(0.2, 0.8, flowPhase));
    fragColor = mix(albedo, flowAlbedo, flowAmount);
}
"#
}
