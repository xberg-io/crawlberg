//! Candidate splitting for `srcset`-shaped attribute values used by image discovery.

/// Split a `srcset`-style list into its candidates, each a URL and its descriptor.
///
/// ~keep Follows the HTML "parse a srcset attribute" steps: a candidate URL is a run of
/// ~keep non-ASCII-whitespace, trailing commas on the URL end the candidate, and otherwise
/// ~keep the descriptor runs to the next comma outside parentheses.
pub(super) fn srcset_candidates(list: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = list;
    std::iter::from_fn(move || {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            return None;
        }
        let url_end = rest.find(|c: char| c.is_ascii_whitespace()).unwrap_or(rest.len());
        let (candidate_url, after_url) = rest.split_at(url_end);
        if candidate_url.ends_with(',') {
            rest = after_url;
            return Some((candidate_url.trim_end_matches(','), ""));
        }
        let (descriptor, remainder) = split_descriptor(after_url);
        rest = remainder;
        Some((candidate_url, descriptor))
    })
}

/// Split a candidate's descriptor from the remaining candidates.
///
/// ~keep Parentheses do not nest: the tokenizer returns to its normal state at the first `)`.
fn split_descriptor(after_url: &str) -> (&str, &str) {
    let is_space = |c: char| c.is_ascii_whitespace();
    let after_url = after_url.trim_start_matches(is_space);
    let mut in_parentheses = false;
    for (index, byte) in after_url.bytes().enumerate() {
        match byte {
            b'(' => in_parentheses = true,
            b')' => in_parentheses = false,
            b',' if !in_parentheses => {
                return (after_url[..index].trim_end_matches(is_space), &after_url[index..]);
            }
            _ => {}
        }
    }
    let descriptor = if in_parentheses {
        after_url
    } else {
        after_url.trim_end_matches(is_space)
    };
    (descriptor, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls_from_their_descriptors() {
        assert_eq!(
            srcset_candidates("a.png 1x, /b.png 2x,https://cdn.example/c.png 3x").collect::<Vec<_>>(),
            [("a.png", "1x"), ("/b.png", "2x"), ("https://cdn.example/c.png", "3x")]
        );
    }

    #[test]
    fn a_data_url_candidate_keeps_its_own_comma() {
        assert_eq!(
            srcset_candidates("data:image/gif;base64,R0lGOD 1x, big.png 800w").collect::<Vec<_>>(),
            [("data:image/gif;base64,R0lGOD", "1x"), ("big.png", "800w")]
        );
    }

    #[test]
    fn selects_the_real_second_candidate_after_a_parenthesised_comma() {
        let candidates = srcset_candidates("a.png 1x (x, y.png 9x ), b.png 2x").collect::<Vec<_>>();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1], ("b.png", "2x"));
    }
}
