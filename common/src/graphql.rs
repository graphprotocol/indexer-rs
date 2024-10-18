// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use regex::Regex;

#[allow(clippy::too_long_first_doc_paragraph)]
/// There is no convenient function for filtering GraphQL executable documents
/// For sake of simplicity, use regex to filter graphql query string
/// Return original string if the query is okay, otherwise error out with  unsupported fields
pub fn filter_supported_fields(
    query: &str,
    supported_root_fields: &HashSet<&str>,
) -> Result<String, Vec<String>> {
    // Create a regex pattern to match the fields not in the supported fields
    let re = Regex::new(r"\b(\w+)\s*\{").unwrap();
    let mut unsupported_fields = Vec::new();

    for cap in re.captures_iter(query) {
        if let Some(match_) = cap.get(1) {
            let field = match_.as_str();
            if !supported_root_fields.contains(field) {
                unsupported_fields.push(field.to_string());
            }
        }
    }

    if !unsupported_fields.is_empty() {
        return Err(unsupported_fields);
    }

    Ok(query.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_supported_fields_with_valid_fields() {
        let supported_fields = vec![
            "indexingStatuses",
            "publicProofsOfIndexing",
            "entityChangesInBlock",
        ]
        .into_iter()
        .collect::<HashSet<&str>>();

        let query_string = "{
            indexingStatuses {
                subgraph
                health
            }
            publicProofsOfIndexing {
                number
            }
        }";

        assert_eq!(
            filter_supported_fields(query_string, &supported_fields).unwrap(),
            query_string.to_string()
        );
    }

    #[test]
    fn test_filter_supported_fields_with_unsupported_fields() {
        let supported_fields = vec![
            "indexingStatuses",
            "publicProofsOfIndexing",
            "entityChangesInBlock",
        ]
        .into_iter()
        .collect::<HashSet<&str>>();

        let query_string = "{
            someField {
                subfield1
                subfield2
            }
            indexingStatuses {
                subgraph
                health
            }
        }";

        let filtered = filter_supported_fields(query_string, &supported_fields);
        assert!(filtered.is_err(),);
        let errors = filtered.err().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors.first().unwrap(), &String::from("someField"));
    }
}
