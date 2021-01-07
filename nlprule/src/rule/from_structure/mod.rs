use crate::types::*;
use crate::{filter::get_filter, utils, utils::regex::SerializeRegex, Error};
use lazy_static::lazy_static;
use onig::Regex;
use std::{collections::HashMap, convert::TryFrom};

mod structure;

pub use structure::{read_disambiguation_rules, read_rules};

use super::disambiguation::*;
use super::engine::composition::concrete::*;
use super::engine::composition::*;
use super::engine::*;
use super::grammar::*;
use super::{DisambiguationRule, Rule};

// TODO: should be an option in config OR restricted to one sentence
fn max_matches() -> usize {
    20
}

fn parse_match_attribs(
    attribs: impl structure::MatchAttributes,
    text: Option<&str>,
    case_sensitive: bool,
    text_match_idx: Option<usize>,
) -> Result<Atom, Error> {
    let mut atoms: Vec<Atom> = Vec::new();

    let case_sensitive = if let Some(case_sensitive) = attribs.case_sensitive() {
        match case_sensitive.as_str() {
            "yes" => true,
            "no" => false,
            x => panic!("unknown case_sensitive value {}", x),
        }
    } else {
        case_sensitive
    };

    let inflected = if let Some(inflected) = attribs.inflected() {
        match inflected.as_str() {
            "yes" => true,
            "no" => false,
            x => panic!("unknown inflected value {}", x),
        }
    } else {
        false
    };

    let is_regex = if let Some(regexp) = attribs.regexp() {
        match regexp.as_str() {
            "yes" => true,
            x => panic!("unknown regexp value {}", x),
        }
    } else {
        false
    };

    // TODO: also reformat is_regex etc., maybe macro?
    let is_postag_regexp = match attribs.postag_regexp().as_deref() {
        Some("yes") => true,
        None => false,
        x => panic!("unknown is_postag_regexp value {:?}", x),
    };

    let negate = match attribs.negate().as_deref() {
        Some("yes") => true,
        None => false,
        x => panic!("unknown negate value {:?}", x),
    };

    let negate_pos = match attribs.negate_pos().as_deref() {
        Some("yes") => true,
        None => false,
        x => panic!("unknown negate_pos value {:?}", x),
    };

    let mut inflect_matcher = None;
    let mut pos_matcher = None;

    if text.is_some() || text_match_idx.is_some() {
        let matcher = if is_regex && text_match_idx.is_none() {
            let regex = SerializeRegex::new(text.unwrap().trim(), true, case_sensitive);
            Matcher::new_regex(regex?, negate, inflected)
        } else {
            Matcher::new_string(
                text_match_idx.map_or_else(
                    || either::Left(text.unwrap().trim().to_string().into()),
                    either::Right,
                ),
                negate,
                case_sensitive,
                inflected,
            )
        };

        if inflected {
            inflect_matcher = Some(matcher);
        } else {
            atoms.push(TextAtom::new(matcher).into());
        }
    }

    if let Some(postag) = attribs.postag() {
        pos_matcher = Some(if is_postag_regexp {
            let regex = SerializeRegex::new(&postag.trim(), true, true);
            Matcher::new_regex(regex?, negate_pos, true)
        } else {
            Matcher::new_string(
                either::Left(postag.trim().to_string().into()),
                negate_pos,
                true,
                true,
            )
        });
    }

    if pos_matcher.is_some() || inflect_matcher.is_some() {
        let matcher = WordDataMatcher::new(pos_matcher, inflect_matcher);
        atoms.push(WordDataAtom::new(matcher, case_sensitive).into());
    }

    match (attribs.chunk(), attribs.chunk_re()) {
        (Some(chunk), None) => {
            let chunk_atom = ChunkAtom::new(Matcher::new_string(
                either::Left(chunk.trim().to_string().into()),
                false,
                true,
                true,
            ));
            atoms.push(chunk_atom.into());
        }
        (None, Some(chunk_re)) => {
            let regex = SerializeRegex::new(chunk_re.trim(), true, true)?;
            let chunk_atom = ChunkAtom::new(Matcher::new_regex(regex, false, true));
            atoms.push(chunk_atom.into());
        }
        (None, None) => {}
        _ => panic!("unexpected combination of chunk / chunk_re values."),
    }

    if let Some(chunk) = attribs.chunk() {
        let chunk_atom = ChunkAtom::new(Matcher::new_string(
            either::Left(chunk.trim().to_string().into()),
            false,
            true,
            true,
        ));

        atoms.push(chunk_atom.into());
    }

    if let Some(space_before) = attribs.spacebefore() {
        let value = match space_before.as_str() {
            "yes" => true,
            "no" => false,
            _ => panic!("unknown spacebefore value {}", space_before),
        };

        atoms.push(SpaceBeforeAtom::new(value).into());
    }

    Ok(AndAtom::and(atoms))
}

fn get_exceptions(
    token: &structure::Token,
    case_sensitive: bool,
    only_shifted: bool,
) -> Result<Atom, Error> {
    if let Some(parts) = &token.parts {
        let exceptions: Vec<Atom> = parts
            .iter()
            .filter_map(|x| match x {
                structure::TokenPart::Exception(x) => Some(x),
                _ => None,
            })
            .filter_map(|x| {
                let exception_text = if let Some(exception_text) = &x.text {
                    Some(exception_text.as_str())
                } else {
                    None
                };
                let mut atom =
                    parse_match_attribs(x, exception_text, case_sensitive, None).unwrap();

                let offset = if let Some(scope) = &x.scope {
                    match scope.as_str() {
                        "next" => 1,
                        "current" => 0,
                        "previous" => -1,
                        _ => panic!("unknown scope value {}", scope),
                    }
                } else {
                    0
                };

                if offset != 0 {
                    atom = OffsetAtom::new(atom, offset).into();
                }

                if !only_shifted || (offset != 0) {
                    Some(atom)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        Ok(NotAtom::not(OrAtom::or(exceptions)))
    } else {
        Ok((TrueAtom {}).into())
    }
}

fn parse_token(token: &structure::Token, case_sensitive: bool) -> Result<Vec<Part>, Error> {
    let mut parts = Vec::new();
    let text = if let Some(parts) = &token.parts {
        parts.iter().find_map(|x| match x {
            structure::TokenPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
    } else {
        None
    };

    let text_match_idx = if let Some(parts) = &token.parts {
        parts.iter().find_map(|x| match x {
            structure::TokenPart::Sub(sub) => Some(sub.no.parse::<usize>().unwrap() + 1),
            _ => None,
        })
    } else {
        None
    };

    let min = token
        .min
        .clone()
        .map(|x| {
            if x == "-1" {
                max_matches()
            } else {
                x.parse().expect("can't parse min as usize")
            }
        })
        .unwrap_or(1usize);
    let mut max = token
        .max
        .clone()
        .map(|x| {
            if x == "-1" {
                max_matches()
            } else {
                x.parse().expect("can't parse max as usize")
            }
        })
        .unwrap_or(1usize);
    if min > 1 && max == 1 {
        max = max_matches();
    }

    let quantifier = Quantifier::new(min, max);
    let mut atom = parse_match_attribs(token, text, case_sensitive, text_match_idx)?;
    atom = AndAtom::and(vec![atom, get_exceptions(token, case_sensitive, false)?]);

    parts.push(Part::new(atom, quantifier, true));

    if let Some(to_skip) = token.skip.clone() {
        let to_skip = if to_skip == "-1" {
            max_matches()
        } else {
            to_skip.parse().expect("can't parse skip as usize or -1")
        };

        parts.push(Part::new(
            get_exceptions(token, case_sensitive, true)?,
            Quantifier::new(0, to_skip),
            false,
        ));
    }

    Ok(parts)
}

fn parse_match(m: structure::Match, composition: &Option<&Composition>) -> Result<Match, Error> {
    if m.postag.is_some()
        || m.postag_regex.is_some()
        || m.postag_replace.is_some()
        || m.text.is_some()
    {
        return Err(Error::Unimplemented(
            "postag, postag_regex, postag_replace and text in `match` are not implemented.".into(),
        ));
    }

    if m.include_skipped.is_some() {
        return Err(Error::Unimplemented(
            "include_skipped in `match` is not implemented.".into(),
        ));
    }

    let mut id =
        m.no.parse::<usize>()
            .expect("no must be parsable as usize.");

    if let Some(composition) = composition {
        let last_id = get_last_id(&composition.parts) as usize - 1;

        if id > last_id {
            id = last_id;
        }
    }

    let case_conversion = if let Some(conversion) = &m.case_conversion {
        Some(conversion.as_str())
    } else {
        None
    };

    let pos_replacer = if let Some(postag) = m.postag {
        if postag.contains("+DT") || postag.contains("+INDT") {
            return Err(Error::Unimplemented(
                "+DT and +INDT determiners are not implemented.".into(),
            ));
        }

        let matcher = match m.postag_regex.as_deref() {
            Some("yes") => {
                let regex = SerializeRegex::new(&postag, true, false)?;
                Matcher::new_regex(regex, false, true)
            }
            None => Matcher::new_string(either::Left(postag.into()), false, false, true),
            x => panic!("unknown postag_regex value {:?}", x),
        };
        Some(PosReplacer::new(matcher))
    } else {
        None
    };

    let regex_replacer = match (m.regexp_match, m.regexp_replace) {
        (Some(regex_match), Some(regex_replace)) => Some((
            SerializeRegex::new(&regex_match, false, true)?,
            regex_replace,
        )),
        _ => None,
    };

    Ok(Match::new(
        id,
        match case_conversion {
            Some("alllower") => Conversion::AllLower,
            Some("startlower") => Conversion::StartLower,
            Some("startupper") => Conversion::StartUpper,
            Some("allupper") => Conversion::AllUpper,
            Some(x) => {
                return Err(Error::Unimplemented(format!(
                    "case conversion {} not supported.",
                    x
                )))
            }
            None => Conversion::Nop,
        },
        pos_replacer,
        regex_replacer,
    ))
}

fn parse_synthesizer_text(text: &str) -> Vec<SynthesizerPart> {
    lazy_static! {
        static ref MATCH_REGEX: Regex = Regex::new(r"\\(\d)").unwrap();
    }

    let mut parts = Vec::new();
    let mut end_index = 0;

    for capture in MATCH_REGEX.captures_iter(&text) {
        let (start, end) = capture.pos(0).unwrap();

        if end_index != start {
            parts.push(SynthesizerPart::Text((&text[end_index..start]).to_string()))
        }

        let index = capture
            .at(1)
            .unwrap()
            .parse::<usize>()
            .expect("match regex capture must be parsable as usize.");

        parts.push(SynthesizerPart::Match(Match::new(
            index,
            Conversion::Nop,
            None,
            None,
        )));
        end_index = end;
    }

    if end_index < text.len() {
        parts.push(SynthesizerPart::Text((&text[end_index..]).to_string()))
    }
    parts
}

fn parse_suggestion(
    data: structure::Suggestion,
    composition: &Option<&Composition>,
) -> Result<Synthesizer, Error> {
    let mut parts = Vec::new();
    for part in data.parts {
        match part {
            structure::SuggestionPart::Text(text) => {
                parts.extend(parse_synthesizer_text(text.as_str()));
            }
            structure::SuggestionPart::Match(m) => {
                parts.push(SynthesizerPart::Match(parse_match(m, composition)?));
            }
        }
    }

    Ok(Synthesizer {
        parts,
        // use titlecase adjustment (i. e. make replacement title case if match is title case) if token rule
        use_titlecase_adjust: composition.is_some(),
    })
}

fn get_last_id(parts: &[Part]) -> isize {
    parts.iter().fold(1, |a, x| a + x.visible as isize)
}

fn parse_parallel_tokens(
    tokens: &[structure::Token],
    case_sensitive: bool,
) -> Result<Vec<Atom>, Error> {
    tokens
        .iter()
        .map(|x| {
            let mut parsed = parse_token(x, case_sensitive)?;

            if parsed.len() != 1 || parsed[0].quantifier.min != 1 || parsed[0].quantifier.max != 1 {
                return Err(Error::Unimplemented(
                    "control flow in parallel tokens is not implemented.".into(),
                ));
            }

            Ok(parsed.remove(0).atom)
        })
        .collect()
}

fn parse_unify_tokens(
    tokens: &[structure::UnifyTokenCombination],
    case_sensitive: bool,
) -> Result<Vec<Part>, Error> {
    let mut out = Vec::new();

    for token_combination in tokens {
        out.extend(match token_combination {
            structure::UnifyTokenCombination::Token(token) => parse_token(token, case_sensitive)?,
            structure::UnifyTokenCombination::And(tokens) => {
                let atom = AndAtom::and(parse_parallel_tokens(&tokens.tokens, case_sensitive)?);
                vec![Part::new(atom, Quantifier::new(1, 1), true)]
            }
            structure::UnifyTokenCombination::Or(tokens) => {
                let atom = OrAtom::or(parse_parallel_tokens(&tokens.tokens, case_sensitive)?);
                vec![Part::new(atom, Quantifier::new(1, 1), true)]
            }
            structure::UnifyTokenCombination::Feature(_) => vec![],
            structure::UnifyTokenCombination::Ignore(ignore) => {
                parse_tokens(&ignore.tokens, case_sensitive)?
            }
        });
    }

    Ok(out)
}

fn parse_tokens(
    tokens: &[structure::TokenCombination],
    case_sensitive: bool,
) -> Result<Vec<Part>, Error> {
    let mut out = Vec::new();

    for token_combination in tokens {
        out.extend(match token_combination {
            structure::TokenCombination::Token(token) => parse_token(token, case_sensitive)?,
            structure::TokenCombination::And(tokens) => {
                let atom = AndAtom::and(parse_parallel_tokens(&tokens.tokens, case_sensitive)?);
                vec![Part::new(atom, Quantifier::new(1, 1), true)]
            }
            structure::TokenCombination::Or(tokens) => {
                let atom = OrAtom::or(parse_parallel_tokens(&tokens.tokens, case_sensitive)?);
                vec![Part::new(atom, Quantifier::new(1, 1), true)]
            }
            structure::TokenCombination::Unify(unify) => {
                parse_unify_tokens(&unify.tokens, case_sensitive)?
            }
        });
    }

    Ok(out)
}

fn parse_pattern(pattern: structure::Pattern) -> Result<(Composition, usize, usize), Error> {
    let mut start = None;
    let mut end = None;

    let mut composition_parts = Vec::new();
    let case_sensitive = match &pattern.case_sensitive {
        Some(string) => string == "yes",
        None => false,
    };

    for part in &pattern.parts {
        match part {
            structure::PatternPart::Token(token) => {
                composition_parts.extend(parse_token(token, case_sensitive)?)
            }
            structure::PatternPart::Marker(marker) => {
                start = Some(get_last_id(&composition_parts));

                composition_parts.extend(parse_tokens(&marker.tokens, case_sensitive)?);

                end = Some(get_last_id(&composition_parts));
            }
            structure::PatternPart::And(tokens) => {
                let atom = AndAtom::and(parse_parallel_tokens(&tokens.tokens, case_sensitive)?);

                composition_parts.push(Part::new(atom, Quantifier::new(1, 1), true));
            }
            structure::PatternPart::Or(tokens) => {
                let atom = OrAtom::or(parse_parallel_tokens(&tokens.tokens, case_sensitive)?);

                composition_parts.push(Part::new(atom, Quantifier::new(1, 1), true));
            }
            structure::PatternPart::Unify(unify) => {
                composition_parts.extend(parse_unify_tokens(&unify.tokens, case_sensitive)?)
            }
        }
    }

    let start = start.unwrap_or(1) as usize;
    let end = end.unwrap_or_else(|| get_last_id(&composition_parts)) as usize;

    let composition = Composition::new(composition_parts);

    Ok((composition, start, end))
}

impl TryFrom<structure::Rule> for Rule {
    type Error = Error;

    fn try_from(data: structure::Rule) -> Result<Rule, Self::Error> {
        if data.filter.is_some() {
            return Err(Error::Unimplemented(
                "rules with filter are not implemented.".into(),
            ));
        }

        let (engine, start, end) = match (data.pattern, data.regex) {
            (Some(_), Some(_)) => Err(Error::Unexpected(
                "must not contain both `pattern` and `regexp`.".into(),
            )),
            (None, None) => Err(Error::Unexpected(
                "either `pattern` or `regexp` must be supplied.".into(),
            )),
            (Some(pattern), None) => {
                let (composition, start, end) = parse_pattern(pattern)?;

                Ok((
                    Engine::Token(TokenEngine {
                        composition,
                        antipatterns: if let Some(antipatterns) = data.antipatterns {
                            antipatterns
                                .into_iter()
                                .map(|pattern| parse_pattern(pattern).map(|x| x.0))
                                .collect::<Result<Vec<_>, Error>>()?
                        } else {
                            Vec::new()
                        },
                    }),
                    start,
                    end,
                ))
            }
            (None, Some(regex)) => {
                let case_sensitive = match regex.case_sensitive.as_deref() {
                    Some("yes") => true,
                    None => false,
                    x => panic!("unknown case_sensitive value {:?}", x),
                };
                let mark = regex.mark.map_or(0, |x| x.parse().unwrap());
                let regex = SerializeRegex::new(&regex.text, false, case_sensitive)?;
                let id_to_idx: HashMap<usize, usize> =
                    (0..regex.captures_len() + 1).enumerate().collect();
                Ok((Engine::Text(regex, id_to_idx), mark, mark + 1))
            }
        }?;

        let maybe_composition = if let Engine::Token(engine) = &engine {
            Some(&engine.composition)
        } else {
            None
        };

        let mut message_parts = Vec::new();
        let mut suggesters = Vec::new();

        for part in data.message.parts {
            match part {
                structure::MessagePart::Suggestion(suggestion) => {
                    let suggester = parse_suggestion(suggestion.clone(), &maybe_composition)?;
                    // simpler to just parse a second time than cloning the result
                    message_parts.extend(parse_suggestion(suggestion, &maybe_composition)?.parts);
                    suggesters.push(suggester);
                }
                structure::MessagePart::Text(text) => {
                    message_parts.extend(parse_synthesizer_text(text.as_str()));
                }
                structure::MessagePart::Match(m) => {
                    message_parts.push(SynthesizerPart::Match(parse_match(m, &maybe_composition)?));
                }
            }
        }

        if let Some(suggestions) = data.suggestions {
            for suggestion in suggestions {
                suggesters.push(parse_suggestion(suggestion, &maybe_composition)?);
            }
        }

        if suggesters.is_empty() {
            return Err(Error::Unimplemented(
                "rules with no suggestion are not implemented.".into(),
            ));
        }

        assert!(!message_parts.is_empty(), "Rules must have a message.");

        let mut tests = Vec::new();
        for example in &data.examples {
            if example.kind.is_some() {
                return Err(Error::Unimplemented(
                    "examples with `type` (i. e. 'triggers_error') are not implemented.".into(),
                ));
            }

            let mut texts = Vec::new();
            let mut char_length = 0;
            let mut suggestion: Option<Suggestion> = None;

            for part in &example.parts {
                match part {
                    structure::ExamplePart::Text(text) => {
                        texts.push(text.as_str());
                        char_length += text.chars().count();
                    }
                    structure::ExamplePart::Marker(marker) => {
                        if suggestion.is_some() {
                            return Err(Error::Unexpected(
                                "example must have one or zero markers".into(),
                            ));
                        }

                        texts.push(marker.text.as_str());
                        let length = marker.text.chars().count();

                        if let Some(correction_text) = &example.correction {
                            let mut text: Vec<_> =
                                correction_text.split('|').map(|x| x.to_string()).collect();

                            text = if char_length == 0 {
                                // title case if at start
                                text.into_iter()
                                    .map(|x| {
                                        utils::apply_to_first(&x, |c| c.to_uppercase().collect())
                                    })
                                    .collect()
                            } else {
                                text
                            };

                            suggestion = Some(Suggestion {
                                source: "_Test".to_string(),
                                message: "_Test".to_string(),
                                start: char_length,
                                end: char_length + length,
                                text,
                            });
                        }

                        char_length += marker.text.chars().count();
                    }
                }
            }

            tests.push(Test {
                text: texts.join(""),
                suggestion,
            });
        }

        Ok(Rule {
            engine,
            tests,
            start,
            end,
            suggesters,
            message: Synthesizer {
                parts: message_parts,
                use_titlecase_adjust: true,
            },
            id: String::new(),
            on: data.default.map_or(true, |x| x != "off"),
        })
    }
}

fn parse_tag_form(form: &str) -> OwnedWord {
    lazy_static! {
        static ref REGEX: Regex = Regex::new(r"(.+?)\[(.+?)\]").unwrap();
    }

    let captures = REGEX.captures(form).unwrap();
    let text = captures.at(1).unwrap().to_string();
    let tags = captures.at(2).unwrap();

    let tags = tags
        .split(',')
        .filter_map(|x| {
            if x == "</S>" {
                // special symbol, presumably for SENT_END, can be ignored
                return None;
            }

            let parts: Vec<_> = x.split('/').collect();
            if parts.len() < 2 {
                None
            } else {
                Some(OwnedWordData::new(
                    parts[0].to_string(),
                    parts[1].to_string(),
                ))
            }
        })
        .collect();

    OwnedWord { text, tags }
}

impl From<structure::WordData> for OwnedWordData {
    fn from(data: structure::WordData) -> Self {
        OwnedWordData::new(data.lemma.unwrap_or_else(String::new), data.pos)
    }
}

fn parse_pos_filter(postag: &str, postag_regexp: Option<&str>) -> POSFilter {
    match postag_regexp.as_deref() {
        Some("yes") => POSFilter::Regex(SerializeRegex::new(&postag, true, true).unwrap()),
        Some(_) | None => POSFilter::String(postag.to_string()),
    }
}

fn parse_unify(
    unify: &structure::Unify,
    unifications: &Option<Vec<structure::Unification>>,
) -> (Vec<Vec<POSFilter>>, Vec<Option<POSFilter>>, Vec<bool>) {
    let mut filters = Vec::new();
    let mut disambig = Vec::new();
    let mut mask = Vec::new();

    for token_combination in &unify.tokens {
        match token_combination {
            structure::UnifyTokenCombination::Feature(feature) => {
                // TODO: no unwrap
                let unification = unifications
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|x| x.feature == feature.id)
                    .unwrap();

                filters.push(
                    unification
                        .equivalences
                        .iter()
                        .map(|equiv| {
                            parse_pos_filter(
                                &equiv.token.postag,
                                equiv.token.postag_regexp.as_deref(),
                            )
                        })
                        .collect(),
                );
            }
            structure::UnifyTokenCombination::And(tokens)
            | structure::UnifyTokenCombination::Or(tokens) => {
                mask.push(true);
                disambig.push(
                    tokens.tokens[0]
                        .postag
                        .as_ref()
                        .map(|x| parse_pos_filter(x, tokens.tokens[0].postag_regexp.as_deref())),
                )
            }
            structure::UnifyTokenCombination::Token(token) => {
                mask.push(true);
                disambig.push(
                    token
                        .postag
                        .as_ref()
                        .map(|x| parse_pos_filter(x, token.postag_regexp.as_deref())),
                )
            }
            structure::UnifyTokenCombination::Ignore(tokens) => {
                for token_combination in &tokens.tokens {
                    match token_combination {
                        structure::TokenCombination::And(tokens)
                        | structure::TokenCombination::Or(tokens) => {
                            mask.push(false);
                            disambig.push(tokens.tokens[0].postag.as_ref().map(|x| {
                                parse_pos_filter(x, tokens.tokens[0].postag_regexp.as_deref())
                            }))
                        }
                        structure::TokenCombination::Token(token) => {
                            mask.push(false);
                            disambig.push(
                                token
                                    .postag
                                    .as_ref()
                                    .map(|x| parse_pos_filter(x, token.postag_regexp.as_deref())),
                            )
                        }
                        structure::TokenCombination::Unify(_) => {
                            panic!("nested unify not supported")
                        }
                    }
                }
            }
        }
    }

    (filters, disambig, mask)
}

impl TryFrom<structure::DisambiguationRule> for DisambiguationRule {
    type Error = Error;

    fn try_from(data: structure::DisambiguationRule) -> Result<DisambiguationRule, Self::Error> {
        // might need the pattern later so clone it here
        let (composition, start, end) = parse_pattern(data.pattern.clone())?;

        let antipatterns = if let Some(antipatterns) = data.antipatterns {
            antipatterns
                .into_iter()
                .map(|pattern| parse_pattern(pattern).map(|x| x.0))
                .collect::<Result<Vec<_>, Error>>()?
        } else {
            Vec::new()
        };

        let word_datas: Vec<_> = if let Some(wds) = data.disambig.word_datas {
            wds.into_iter()
                .map(|part| match part {
                    structure::DisambiguationPart::WordData(x) => either::Left(x.into()),
                    structure::DisambiguationPart::Match(x) => either::Right(parse_pos_filter(
                        &x.postag.unwrap(),
                        x.postag_regexp.as_deref(),
                    )),
                })
                .collect()
        } else {
            Vec::new()
        };

        let disambiguations = match data.disambig.action.as_deref() {
            Some("remove") => {
                if let Some(postag) = data.disambig.postag.as_ref() {
                    Ok(Disambiguation::Remove(vec![either::Right(
                        parse_pos_filter(postag, Some("yes")),
                    )]))
                } else {
                    Ok(Disambiguation::Remove(word_datas.into_iter().collect()))
                }
            }
            Some("add") => {
                if data.disambig.postag.is_some() {
                    panic!("postag not supported for `add`.")
                }

                Ok(Disambiguation::Add(
                    word_datas
                        .into_iter()
                        .map(|x| x.left().expect("match not supported for `add`"))
                        .collect(),
                ))
            }
            Some("replace") => {
                if data.disambig.postag.is_some() {
                    panic!("postag not supported for `replace`.")
                }

                Ok(Disambiguation::Replace(
                    word_datas
                        .into_iter()
                        .map(|x| {
                            x.left()
                                .expect("match not supported for `replace` disambiguation")
                        })
                        .collect(),
                ))
            }
            Some("ignore_spelling") => Ok(Disambiguation::Nop), // ignore_spelling can be ignored since we dont check spelling
            Some("immunize") => Ok(Disambiguation::Nop), // immunize can probably not be ignored
            Some("filterall") => {
                let mut disambig = Vec::new();
                let mut marker_disambig = Vec::new();
                let mut has_marker = false;

                for part in &data.pattern.parts {
                    match part {
                        structure::PatternPart::Marker(marker) => {
                            has_marker = true;
                            for token in &marker.tokens {
                                let token = match token {
                                    structure::TokenCombination::Token(token) => token,
                                    structure::TokenCombination::And(tokens)
                                    | structure::TokenCombination::Or(tokens) => &tokens.tokens[0],
                                    structure::TokenCombination::Unify(_) => {
                                        panic!("`unify` not supported in `filterall`")
                                    }
                                };

                                marker_disambig.push(token.postag.as_ref().map(|x| {
                                    either::Right(parse_pos_filter(
                                        x,
                                        token.postag_regexp.as_deref(),
                                    ))
                                }));
                            }
                        }
                        structure::PatternPart::Token(token) => {
                            disambig.push(token.postag.as_ref().map(|x| {
                                either::Right(parse_pos_filter(x, token.postag_regexp.as_deref()))
                            }))
                        }
                        structure::PatternPart::And(tokens)
                        | structure::PatternPart::Or(tokens) => {
                            disambig.push(tokens.tokens[0].postag.as_ref().map(|x| {
                                either::Right(parse_pos_filter(
                                    x,
                                    tokens.tokens[0].postag_regexp.as_deref(),
                                ))
                            }))
                        }
                        structure::PatternPart::Unify(_) => {
                            panic!("`unify` not supported in `filterall`")
                        }
                    }
                }

                let disambiguations = if has_marker {
                    marker_disambig
                } else {
                    disambig
                };

                Ok(Disambiguation::Filter(
                    disambiguations.into_iter().collect(),
                ))
            }
            Some("filter") => {
                if let Some(postag) = data.disambig.postag.as_ref() {
                    Ok(Disambiguation::Filter(vec![Some(either::Right(
                        parse_pos_filter(postag, Some("yes")),
                    ))]))
                } else {
                    Ok(Disambiguation::Filter(
                        word_datas.into_iter().map(Some).collect(),
                    ))
                }
            }
            Some("unify") => {
                let mut filters = Vec::new();
                let mut disambig = Vec::new();
                let mut mask = Vec::new();

                match &data.pattern.parts[..] {
                    [.., structure::PatternPart::Marker(marker)] => match &marker.tokens[..] {
                        [structure::TokenCombination::Unify(unify)] => {
                            let (f, d, m) = parse_unify(&unify, &data.unifications);
                            filters.extend(f);
                            disambig.extend(d);
                            mask.extend(m);
                        }
                        _ => panic!("only `unify` as only element in `marker` is implemented"),
                    },
                    [structure::PatternPart::Unify(unify)] => {
                        let (f, d, m) = parse_unify(&unify, &data.unifications);
                        filters.extend(f);
                        disambig.extend(d);
                        mask.extend(m);
                    }
                    _ => panic!("only `unify` as only element in `pattern` is implemented"),
                }

                Ok(Disambiguation::Unify(filters, disambig, mask))
            }
            None => {
                if let Some(postag) = data.disambig.postag.as_ref() {
                    Ok(Disambiguation::Filter(vec![Some(either::Left(
                        OwnedWordData::new(String::new(), postag.to_string()),
                    ))]))
                } else {
                    Ok(Disambiguation::Filter(
                        word_datas.into_iter().map(Some).collect(),
                    ))
                }
            }
            Some(x) => Err(Error::Unimplemented(format!("action {}", x))),
        }?;

        let filter = if let Some(filter_data) = data.filter {
            let args = filter_data
                .args
                .split(' ')
                .map(|x| {
                    let idx = x.find(':').unwrap();
                    (
                        x[..idx].to_string(),
                        x[(idx + ':'.len_utf8())..].to_string(),
                    )
                })
                .collect();

            Some(get_filter(
                filter_data.class.split('.').next_back().unwrap(),
                args,
            )?)
        } else {
            None
        };

        let mut tests = Vec::new();

        if let Some(examples) = data.examples.as_ref() {
            for example in examples {
                let mut texts = Vec::new();
                let mut char_span: Option<(usize, usize)> = None;
                let mut char_length = 0;

                for part in &example.parts {
                    match part {
                        structure::ExamplePart::Text(text) => {
                            texts.push(text.as_str());
                            char_length += text.chars().count();
                        }
                        structure::ExamplePart::Marker(marker) => {
                            if char_span.is_some() {
                                return Err(Error::Unexpected(
                                    "example must have one or zero markers".into(),
                                ));
                            }

                            texts.push(marker.text.as_str());
                            let length = marker.text.chars().count();

                            char_span = Some((char_length, char_length + length));

                            char_length += marker.text.chars().count();
                        }
                    }
                }

                let text = texts.join("");

                let test = match example.kind.as_str() {
                    "untouched" => DisambiguationTest::Unchanged(text),
                    "ambiguous" => DisambiguationTest::Changed(DisambiguationChange {
                        text,
                        before: parse_tag_form(
                            example
                                .inputform
                                .as_ref()
                                .expect("must have inputform when ambiguous example"),
                        ),
                        after: parse_tag_form(
                            &example
                                .outputform
                                .as_ref()
                                .expect("must have inputform when ambiguous example"),
                        ),
                        char_span: char_span.expect("must have marker when ambiguous example"),
                    }),
                    x => panic!("unknown disambiguation example type {}", x),
                };

                tests.push(test);
            }
        }

        Ok(DisambiguationRule {
            engine: Engine::Token(TokenEngine {
                composition,
                antipatterns,
            }),
            filter,
            disambiguations,
            start,
            end,
            tests,
            id: String::new(),
        })
    }
}