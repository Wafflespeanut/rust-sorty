use rustc::lint::{EarlyContext, EarlyLintPass, LintArray, LintContext, LintPass};
use std::cmp::Ordering;
use syntax::ast::{Item, ItemKind, LitKind, MetaItemKind, Mod, NodeId};
use syntax::ast::{NestedMetaItemKind, ViewPath_, Visibility};
use syntax::codemap::Span;
use syntax::print::pprust::path_to_string;
use syntax::symbol::keywords;

// Warn unsorted declarations by default (since denying is a poor choice for styling lints)
declare_lint!(UNSORTED_DECLARATIONS, Warn,
              "Warn when the declarations of crates or modules are not in alphabetical order");

pub struct Sorty;

impl LintPass for Sorty {
    fn get_lints(&self) -> LintArray {
        lint_array!(UNSORTED_DECLARATIONS)
    }
}

impl EarlyLintPass for Sorty {
    // Walking through all the modules is enough for our purpose
    fn check_mod(&mut self, cx: &EarlyContext, module: &Mod, _span: Span, _id: NodeId) {
        // TODO: lint should stop ignoring the comments near the declarations
        let session_codemap = cx.sess.codemap();    // required only for checking inline mods
        let mut extern_crates = Vec::new();
        let mut uses = Vec::new();
        let mut mods = Vec::new();

        for item in &module.items {
            // I've made use of `format!` most of the time, because we have a mixture of
            // `String` & `InternedString`
            let item_name = format!("{}", item.ident.name.as_str());
            let item_span = item.span;
            match item.node {
                ItemKind::ExternCrate(ref optional_name) if item_name != "std" => {
                    // We've put the declaration here because, we have to sort crate declarations
                    // with respect to the renamed version (instead of the old one).
                    // Since we also don't have `pub` (indicated by the `false` below),
                    // we could just append the declaration to the attributes.
                    let mut item_attrs = get_item_attrs(&item, false);
                    item_attrs = match *optional_name {    // for `extern crate foo as bar`
                        Some(ref old_name) => format!("{}extern crate {} as", item_attrs, old_name),
                        None => format!("{}extern crate", item_attrs),
                    };

                    extern_crates.push((item_name, item_attrs, item_span, false));
                }

                ItemKind::Mod(ref module) => {
                    let mod_invoked_file = session_codemap.span_to_filename(item.span);
                    let mod_declared_file = session_codemap.span_to_filename(module.inner);
                    if mod_declared_file != mod_invoked_file {          // ignores inline modules
                        let item_attrs = get_item_attrs(&item, true);
                        mods.push((item_name, item_attrs, item_span, false));
                    }
                }

                ItemKind::Use(ref spanned) => {
                    let item_attrs = get_item_attrs(&item, true);
                    match spanned.node {
                        ViewPath_::ViewPathSimple(ref ident, ref path) => {
                            let path_str = path_to_string(&path);
                            let name = ident.name.as_str();

                            let renamed = {     // `use foo as bar`
                                let split = path_str.split(":").collect::<Vec<_>>();
                                match split[split.len() - 1] == &*name {
                                    true => path_str.clone(),
                                    false => format!("{} as {}", &path_str, &name),
                                }
                            };

                            uses.push((renamed, item_attrs, item_span, false));
                        }

                        ViewPath_::ViewPathList(ref path, ref list) => {
                            let old_list = list.iter().map(|&list_item| {
                                if list_item.node.name.name == keywords::SelfValue.name() {
                                    "self".to_owned()   // this must be `self`
                                } else {
                                    let name = list_item.node.name.name.as_str();
                                    match list_item.node.rename {
                                        Some(new_name) => format!("{} as {}", name, new_name),
                                        None => (&*name).to_owned(),
                                    }
                                }
                            }).collect::<Vec<_>>();

                            let mut new_list = old_list.clone();
                            new_list.sort_by(|a, b| {
                                match (&**a, &**b) {    // `self` should be first in a list of use items
                                    ("self", _) => Ordering::Less,
                                    (_, "self") => Ordering::Greater,
                                    _ => a.cmp(b),
                                }
                            });

                            let mut warn = false;
                            let use_list = format!("{}::{{{}}}", path_to_string(&path), new_list.join(", "));
                            for (old_stuff, new_stuff) in old_list.iter().zip(new_list.iter()) {
                                // check whether the use list is sorted
                                if old_stuff != new_stuff {
                                    warn = true;
                                    break
                                }
                            }

                            uses.push((use_list, item_attrs, path.span, warn));
                        }

                        ViewPath_::ViewPathGlob(ref path) => {
                            let path_str = path_to_string(&path) + "::*";
                            // We don't have any use statements like `use std::prelude::*`
                            // since it's done only by rustc, we can safely neglect those here
                            if !path_str.starts_with("std::") {
                                uses.push((path_str, item_attrs, item_span, false));
                            }
                        }
                    }
                }
                _ => (),
            }
        }

        // We don't include the crate declaration here, because we've already appended it with the
        // attributes
        check_sort(&extern_crates, cx, "crate declarations", "");
        check_sort(&mods, cx, "module declarations (other than inline modules)", "mod");
        check_sort(&uses, cx, "use statements", "use");

        // for collecting, formatting & filtering the attributes (and checking the visibility)
        fn get_item_attrs(item: &Item, pub_check: bool) -> String {
            let mut attr_vec = item.attrs.iter().filter_map(|attr| {
                attr.meta().and_then(|meta| {
                    let meta_string = get_meta_as_string(&meta.name.as_str(), &meta.node);
                    match meta_string.starts_with("doc = ") {
                        true => None,
                        false => Some(format!("#[{}]", meta_string)),
                    }
                })
            }).collect::<Vec<_>>();

            attr_vec.sort_by(|a, b| {
                match (&**a, &**b) {    // put `macro_use` first for later checking
                    ("#[macro_use]", _) => Ordering::Less,
                    (_, "#[macro_use]") => Ordering::Greater,
                    _ => a.cmp(b),
                }
            });

            let attr_string = attr_vec.join("\n");
            match item.vis {
                Visibility::Public if pub_check => match attr_string.is_empty() {
                    true => "pub ".to_owned(),
                    false => attr_string + "\npub ",    // `pub` for mods and uses
                },
                _ => match attr_string.is_empty() {
                    true => attr_string,
                    false => attr_string + "\n",
                },
            }
        }

        fn format_literal(lit: &LitKind) -> String {
            match lit {
                &LitKind::Str(ref inner_str, _) => format!("{}", inner_str),
                _ => panic!("unexpected literal for meta item!"),
            }
        }

        // Collect the information from meta items into Strings
        fn get_meta_as_string(name: &str, meta_item: &MetaItemKind) -> String {
            match *meta_item {
                MetaItemKind::Word => format!("{}", name),
                MetaItemKind::List(ref meta_items) => {
                    let mut stuff = meta_items.iter().map(|meta_item| {
                        match meta_item.node {
                            NestedMetaItemKind::MetaItem(ref meta) =>
                                get_meta_as_string(&meta.name.as_str(), &meta.node),
                            NestedMetaItemKind::Literal(ref value) => format_literal(&value.node),
                        }
                    }).collect::<Vec<_>>();

                    stuff.sort();
                    format!("{}({})", name, stuff.join(", "))
                },
                MetaItemKind::NameValue(ref literal) => {
                    let value = format_literal(&literal.node);
                    format!("{} = \"{}\"", name, value)
                },
            }
        }

        // Checks the sorting of all the declarations and raises warnings whenever necessary
        // takes a slice of tuples with name, related attributes, spans and whether to warn for
        // unordered use lists
        fn check_sort(old_list: &[(String, String, Span, bool)], cx: &EarlyContext,
                      kind: &str, syntax: &str) {

            // prepend given characters to the names for "biased" sorting
            fn str_for_biased_sort(string: &str, choice: bool, prepend_char: &str) -> String {
                match choice {
                    true => prepend_char.to_owned() + string,
                    false => string.to_owned(),
                }
            }

            let mut new_list = old_list.iter().map(|&(ref name, ref attrs, _span, warn)| {
               (name.clone(), attrs.clone(), warn)
            }).collect::<Vec<_>>();

            new_list.sort_by(|&(ref str_a, ref attr_a, _), &(ref str_b, ref attr_b, _)| {
                // move the `pub` statements below
                // (with `~` since it's on the farther side of ASCII)
                let mut new_str_a = str_for_biased_sort(&str_a, attr_a.ends_with("pub "), "~");
                let mut new_str_b = str_for_biased_sort(&str_b, attr_b.ends_with("pub "), "~");
                // move the #[macro_use] stuff above
                // (with `!` since it's on the lower extreme of ASCII)
                new_str_a = str_for_biased_sort(&new_str_a,
                                                attr_a.starts_with("#[macro_use]"), "!");
                new_str_b = str_for_biased_sort(&new_str_b,
                                                attr_b.starts_with("#[macro_use]"), "!");
                new_str_a.cmp(&new_str_b)
            });

            for (i, (&(ref old_name, _, span_start, _warn),
                     &(ref new_name, _, warn))) in old_list.iter()
                                                           .zip(new_list.iter())
                                                           .enumerate() {
                if (old_name != new_name) || warn {
                    // print all the declarations proceeding the first unsorted one
                    let suggestion_list = new_list[i..new_list.len()]
                                          .iter()
                                          .map(|&(ref name, ref attrs, _)| {
                                              format!("{}{} {};", attrs, syntax, name)
                                          }).collect::<Vec<_>>();

                    // increase the span to include more lines
                    let mut final_span = span_start;
                    let (_, _, old_span, _) = old_list[old_list.len() - 1];
                    final_span.hi = old_span.hi;

                    let message = format!("{} should be in alphabetical order!", kind);
                    let suggestion = format!("Try this...\n\n{}\n", suggestion_list.join("\n"));
                    cx.span_lint_help(UNSORTED_DECLARATIONS, final_span, &message, &suggestion);
                    break
                }
            }
        }
    }
}
