use super::{add_column, add_model, OrmAnalysis};
use crate::schema::StaticSource;

#[derive(Clone, Copy)]
enum PrismaLexState {
    Code,
    LineComment,
    BlockComment,
    String,
    Escape,
}

struct PrismaBlock<'a> {
    name: &'a str,
    body: &'a str,
    body_start: usize,
}

enum MappedName {
    Absent,
    Literal(String),
    Dynamic,
}

fn mask_byte(masked: &mut [u8], index: usize) {
    if masked[index].is_ascii() && masked[index] != b'\n' {
        masked[index] = b' ';
    }
}

fn advance_prisma_lexer(
    source: &[u8],
    masked: &mut [u8],
    index: usize,
    state: PrismaLexState,
) -> PrismaLexState {
    let current = source[index];
    let next = source.get(index + 1).copied();
    match state {
        PrismaLexState::Code => match (current, next) {
            (b'/', Some(b'/')) => {
                mask_byte(masked, index);
                PrismaLexState::LineComment
            }
            (b'/', Some(b'*')) => {
                mask_byte(masked, index);
                PrismaLexState::BlockComment
            }
            (b'"', _) => {
                mask_byte(masked, index);
                PrismaLexState::String
            }
            _ => PrismaLexState::Code,
        },
        PrismaLexState::LineComment => {
            if current == b'\n' {
                PrismaLexState::Code
            } else {
                mask_byte(masked, index);
                PrismaLexState::LineComment
            }
        }
        PrismaLexState::BlockComment => {
            mask_byte(masked, index);
            if current == b'*' && next == Some(b'/') {
                mask_byte(masked, index + 1);
                PrismaLexState::Code
            } else {
                PrismaLexState::BlockComment
            }
        }
        PrismaLexState::String => {
            mask_byte(masked, index);
            match current {
                b'\\' => PrismaLexState::Escape,
                b'"' => PrismaLexState::Code,
                _ => PrismaLexState::String,
            }
        }
        PrismaLexState::Escape => {
            mask_byte(masked, index);
            PrismaLexState::String
        }
    }
}

fn mask_comments_and_strings(content: &str) -> String {
    let source = content.as_bytes();
    let mut masked = source.to_vec();
    let mut state = PrismaLexState::Code;
    for index in 0..source.len() {
        state = advance_prisma_lexer(source, &mut masked, index, state);
    }
    String::from_utf8(masked).expect("mask preserves UTF-8 boundaries")
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn model_headers(masked: &str) -> Vec<(&str, usize)> {
    let bytes = masked.as_bytes();
    let mut headers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if index + 5 <= bytes.len()
            && &bytes[index..index + 5] == b"model"
            && (index == 0 || !identifier_byte(bytes[index - 1]))
        {
            let after_keyword = index + 5;
            if after_keyword < bytes.len() && !identifier_byte(bytes[after_keyword]) {
                let mut name_start = after_keyword;
                while name_start < bytes.len() && bytes[name_start].is_ascii_whitespace() {
                    name_start += 1;
                }
                let mut name_end = name_start;
                while name_end < bytes.len() && identifier_byte(bytes[name_end]) {
                    name_end += 1;
                }
                let mut brace = name_end;
                while brace < bytes.len() && bytes[brace].is_ascii_whitespace() {
                    brace += 1;
                }
                if name_end > name_start && brace < bytes.len() && bytes[brace] == b'{' {
                    headers.push((&masked[name_start..name_end], brace));
                    index = brace;
                }
            }
        }
        index += 1;
    }
    headers
}

fn matching_brace(masked: &str, open: usize) -> Option<(usize, usize)> {
    let bytes = masked.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((open, index));
                }
            }
            _ => {}
        }
    }
    None
}

fn model_blocks<'a>(masked: &'a str, original: &'a str) -> Vec<PrismaBlock<'a>> {
    let mut blocks = Vec::new();
    for (name, open) in model_headers(masked) {
        if let Some((open, close)) = matching_brace(masked, open) {
            if original.is_char_boundary(open + 1) && original.is_char_boundary(close) {
                blocks.push(PrismaBlock {
                    name,
                    body: &original[open + 1..close],
                    body_start: open + 1,
                });
            }
        }
    }
    blocks
}

fn find_attribute<'a>(field_line: &'a str, name: &str) -> Option<&'a str> {
    let masked = mask_comments_and_strings(field_line);
    let bytes = masked.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'@' if depth == 0 => {
                let name_start = index + 1;
                let mut name_end = name_start;
                while name_end < bytes.len() && identifier_byte(bytes[name_end]) {
                    name_end += 1;
                }
                if &bytes[name_start..name_end] != name.as_bytes() {
                    index = name_end;
                    continue;
                }
                let mut open = name_end;
                while open < bytes.len() && bytes[open].is_ascii_whitespace() {
                    open += 1;
                }
                if open >= bytes.len() || bytes[open] != b'(' {
                    return None;
                }
                let mut nested = 1usize;
                for close in open + 1..bytes.len() {
                    match bytes[close] {
                        b'(' => nested += 1,
                        b')' => {
                            nested -= 1;
                            if nested == 0 {
                                return field_line.get(open + 1..close);
                            }
                        }
                        _ => {}
                    }
                }
                return None;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn mapped_name(field_line: &str) -> MappedName {
    let Some(value) = find_attribute(field_line, "map").map(str::trim) else {
        return MappedName::Absent;
    };
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        let mut escaped = false;
        for character in inner.chars() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                return MappedName::Dynamic;
            }
        }
        return MappedName::Literal(inner.to_string());
    }
    MappedName::Dynamic
}

fn line_of(content: &str, offset: usize) -> u32 {
    content[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
        + 1
}

pub(super) fn analyze(analysis: &mut OrmAnalysis, path: &str, content: &str) {
    let masked = mask_comments_and_strings(content);
    for block in model_blocks(&masked, content) {
        let model_line = line_of(content, block.body_start);
        let mut table = block.name.to_string();
        let mut dynamic_table = false;
        for line in block.body.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("@@map") {
                match mapped_name(trimmed) {
                    MappedName::Literal(name) => table = name,
                    MappedName::Dynamic => dynamic_table = true,
                    MappedName::Absent => {}
                }
            }
        }
        if dynamic_table {
            analysis
                .unresolved
                .push(format!("{path}: dynamic table name on {}", block.name));
            continue;
        }
        add_model(
            analysis,
            "prisma",
            block.name,
            &table,
            path,
            model_line,
            StaticSource::Regex,
        );
        let masked_body = &masked[block.body_start..block.body_start + block.body.len()];
        for (offset, line) in block.body.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("@@")
                || masked_body
                    .lines()
                    .nth(offset)
                    .is_none_or(|line| line.trim().is_empty())
            {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let Some(field) = parts.next() else {
                continue;
            };
            if parts.next().is_none() {
                continue;
            }
            match mapped_name(trimmed) {
                MappedName::Literal(column) => add_column(
                    analysis,
                    &table,
                    &column,
                    path,
                    model_line + offset as u32 + 1,
                    StaticSource::Regex,
                ),
                MappedName::Absent => add_column(
                    analysis,
                    &table,
                    field,
                    path,
                    model_line + offset as u32 + 1,
                    StaticSource::Regex,
                ),
                MappedName::Dynamic => analysis
                    .unresolved
                    .push(format!("{path}: dynamic column on {}", block.name)),
            }
        }
    }
}
