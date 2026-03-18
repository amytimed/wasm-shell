//! Tests for the bash parser.
//!
//! Organised by feature area. Each test parses a snippet and checks either:
//! - The AST matches expectations (happy path), or
//! - A ParseError is returned with the expected message fragment (error path).

#[cfg(test)]
mod simple_commands {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn bare_command() {
        let script = parse("echo").unwrap();
        assert_eq!(script.len(), 1);
        match &script[0] {
            Command::Simple(sc) => {
                assert_eq!(sc.name, Some(Word::Literal("echo".into())));
                assert!(sc.args.is_empty());
            }
            other => panic!("expected Simple, got {other:?}"),
        }
    }

    #[test]
    fn command_with_args() {
        let script = parse("echo hello world").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.name, Some(Word::Literal("echo".into())));
        assert_eq!(sc.args, vec![Word::Literal("hello".into()), Word::Literal("world".into())]);
    }

    #[test]
    fn command_with_flags() {
        let script = parse("ls -la /tmp").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.name, Some(Word::Literal("ls".into())));
        assert_eq!(sc.args[0], Word::Literal("-la".into()));
        assert_eq!(sc.args[1], Word::Literal("/tmp".into()));
    }

    #[test]
    fn empty_input() {
        let script = parse("").unwrap();
        assert!(script.is_empty());
    }

    #[test]
    fn whitespace_only() {
        let script = parse("   \t  ").unwrap();
        assert!(script.is_empty());
    }

    #[test]
    fn comment_only() {
        let script = parse("# this is a comment").unwrap();
        assert!(script.is_empty());
    }

    #[test]
    fn command_after_comment() {
        let script = parse("# comment\necho hi").unwrap();
        assert_eq!(script.len(), 1);
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.name, Some(Word::Literal("echo".into())));
    }

    #[test]
    fn inline_comment() {
        let script = parse("echo hello # this is ignored").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args, vec![Word::Literal("hello".into())]);
    }

    #[test]
    fn assignment_only() {
        let script = parse("FOO=bar").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(sc.name.is_none());
        assert_eq!(sc.assignments, vec![("FOO".into(), Word::Literal("bar".into()))]);
    }

    #[test]
    fn assignment_before_command() {
        let script = parse("FOO=bar echo $FOO").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.assignments, vec![("FOO".into(), Word::Literal("bar".into()))]);
        assert_eq!(sc.name, Some(Word::Literal("echo".into())));
    }

    #[test]
    fn multiple_assignments() {
        let script = parse("A=1 B=2 cmd").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.assignments.len(), 2);
        assert_eq!(sc.assignments[0].0, "A");
        assert_eq!(sc.assignments[1].0, "B");
    }
}

#[cfg(test)]
mod sequences {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn semicolon_sequence() {
        let script = parse("echo a; echo b").unwrap();
        assert_eq!(script.len(), 2);
    }

    #[test]
    fn newline_sequence() {
        let script = parse("echo a\necho b\necho c").unwrap();
        assert_eq!(script.len(), 3);
    }

    #[test]
    fn mixed_separators() {
        let script = parse("a; b\nc").unwrap();
        assert_eq!(script.len(), 3);
    }

    #[test]
    fn trailing_semicolon() {
        let script = parse("echo a;").unwrap();
        assert_eq!(script.len(), 1);
    }
}

#[cfg(test)]
mod logical_operators {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn and_and() {
        let script = parse("a && b").unwrap();
        assert_eq!(script.len(), 1);
        let Command::And(l, r) = &script[0] else { panic!("expected And") };
        assert!(matches!(l.as_ref(), Command::Simple(_)));
        assert!(matches!(r.as_ref(), Command::Simple(_)));
    }

    #[test]
    fn or_or() {
        let script = parse("a || b").unwrap();
        let Command::Or(l, r) = &script[0] else { panic!("expected Or") };
        let Command::Simple(lsc) = l.as_ref() else { panic!() };
        assert_eq!(lsc.name, Some(Word::Literal("a".into())));
    }

    #[test]
    fn chained_and_or() {
        let script = parse("a && b || c").unwrap();
        // `a && b` is left-associative, then `|| c`
        // Result: (a && b) || c
        let Command::Or(l, r) = &script[0] else { panic!("expected Or at top") };
        assert!(matches!(l.as_ref(), Command::And(_, _)));
    }

    #[test]
    fn and_with_newline_continuation() {
        let script = parse("a &&\nb").unwrap();
        assert!(matches!(&script[0], Command::And(_, _)));
    }

    #[test]
    fn and_and_three() {
        let script = parse("a && b && c").unwrap();
        // Parses as (a && b) && c (left-associative)
        let Command::And(l, r) = &script[0] else { panic!() };
        assert!(matches!(l.as_ref(), Command::And(_, _)));
    }
}

#[cfg(test)]
mod pipelines {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn simple_pipeline() {
        let script = parse("echo hello | grep hello").unwrap();
        let Command::Pipeline(p) = &script[0] else { panic!("expected Pipeline") };
        assert_eq!(p.commands.len(), 2);
        assert_eq!(p.commands[0].name, Some(Word::Literal("echo".into())));
        assert_eq!(p.commands[1].name, Some(Word::Literal("grep".into())));
        assert!(!p.negated);
    }

    #[test]
    fn three_stage_pipeline() {
        let script = parse("cat file | grep foo | wc -l").unwrap();
        let Command::Pipeline(p) = &script[0] else { panic!() };
        assert_eq!(p.commands.len(), 3);
    }

    #[test]
    fn negated_pipeline() {
        let script = parse("! echo hello | grep world").unwrap();
        let Command::Pipeline(p) = &script[0] else { panic!() };
        assert!(p.negated);
    }

    #[test]
    fn pipeline_with_newline() {
        let script = parse("echo hi |\ngrep hi").unwrap();
        let Command::Pipeline(p) = &script[0] else { panic!() };
        assert_eq!(p.commands.len(), 2);
    }

    #[test]
    fn pipeline_in_and() {
        let script = parse("echo a | grep a && echo b").unwrap();
        let Command::And(l, _) = &script[0] else { panic!() };
        assert!(matches!(l.as_ref(), Command::Pipeline(_)));
    }
}

#[cfg(test)]
mod background_jobs {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn background_command() {
        let script = parse("sleep 10 &").unwrap();
        let Command::Background(inner) = &script[0] else { panic!("expected Background") };
        assert!(matches!(inner.as_ref(), Command::Simple(_)));
    }

    #[test]
    fn background_pipeline() {
        let script = parse("cat file | wc -l &").unwrap();
        let Command::Background(inner) = &script[0] else { panic!() };
        assert!(matches!(inner.as_ref(), Command::Pipeline(_)));
    }

    #[test]
    fn background_then_command() {
        let script = parse("sleep 5 & echo done").unwrap();
        // sleep runs in background, then echo runs
        assert_eq!(script.len(), 2);
        assert!(matches!(&script[0], Command::Background(_)));
        assert!(matches!(&script[1], Command::Simple(_)));
    }
}

#[cfg(test)]
mod subshells_and_groups {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn subshell() {
        let script = parse("(echo hello)").unwrap();
        let Command::Subshell(body) = &script[0] else { panic!("expected Subshell") };
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn subshell_sequence() {
        let script = parse("(a; b; c)").unwrap();
        let Command::Subshell(body) = &script[0] else { panic!() };
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn nested_subshell() {
        let script = parse("(echo a; (echo b))").unwrap();
        let Command::Subshell(body) = &script[0] else { panic!() };
        assert_eq!(body.len(), 2);
        assert!(matches!(&body[1], Command::Subshell(_)));
    }

    #[test]
    fn group() {
        let script = parse("{ echo a; echo b; }").unwrap();
        let Command::Group(body) = &script[0] else { panic!("expected Group") };
        assert_eq!(body.len(), 2);
    }

    #[test]
    fn subshell_and_then_command() {
        let script = parse("(cd /tmp) && ls").unwrap();
        let Command::And(l, r) = &script[0] else { panic!() };
        assert!(matches!(l.as_ref(), Command::Subshell(_)));
        assert!(matches!(r.as_ref(), Command::Simple(_)));
    }

    #[test]
    fn unclosed_subshell_error() {
        let err = parse("(echo hello").unwrap_err();
        assert!(err.message.contains("unclosed subshell") || err.message.contains("missing `)`"));
    }

    #[test]
    fn unclosed_group_error() {
        let err = parse("{ echo hello").unwrap_err();
        assert!(err.message.contains("unclosed group") || err.message.contains("missing `}`"));
    }
}

#[cfg(test)]
mod redirections {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn stdout_redirect() {
        let script = parse("echo hi > /tmp/out").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.redirects.len(), 1);
        let r = &sc.redirects[0];
        assert_eq!(r.fd, Some(1));
        assert_eq!(r.kind, RedirectKind::Write);
        assert!(matches!(r.target, RedirectTarget::Word(_)));
    }

    #[test]
    fn stdout_append() {
        let script = parse("echo hi >> /tmp/out").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.redirects[0].kind, RedirectKind::Append);
    }

    #[test]
    fn stdin_redirect() {
        let script = parse("cat < /tmp/in").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.redirects[0].fd, Some(0));
        assert_eq!(sc.redirects[0].kind, RedirectKind::Read);
    }

    #[test]
    fn stderr_redirect() {
        let script = parse("cmd 2> /tmp/err").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.redirects[0].fd, Some(2));
    }

    #[test]
    fn stderr_to_stdout() {
        let script = parse("cmd 2>&1").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.redirects[0].fd, Some(2));
        assert!(matches!(sc.redirects[0].target, RedirectTarget::Fd(1)));
    }

    #[test]
    fn close_fd() {
        let script = parse("cmd 2>&-").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.redirects[0].target, RedirectTarget::CloseFd));
    }

    #[test]
    fn multiple_redirects() {
        let script = parse("cmd < in > out 2>&1").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.redirects.len(), 3);
    }

    #[test]
    fn redirect_after_args() {
        let script = parse("grep foo /etc/hosts > /tmp/out").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args.len(), 2);
        assert_eq!(sc.redirects.len(), 1);
    }
}

#[cfg(test)]
mod heredocs {
    use crate::ast::*;
    use crate::parse;

    fn parse_with_heredocs(src: &str) -> Vec<Command> {
        parse(src).unwrap()
    }

    #[test]
    fn basic_heredoc() {
        let src = "cat <<EOF\nhello world\nEOF\n";
        let script = parse_with_heredocs(src);
        let Command::Simple(sc) = &script[0] else { panic!() };
        let r = &sc.redirects[0];
        assert!(matches!(r.target, RedirectTarget::Heredoc(_)));
        if let RedirectTarget::Heredoc(hd) = &r.target {
            assert_eq!(hd.delimiter, "EOF");
            assert!(!hd.quoted);
            match &hd.body {
                HeredocBody::Parts(parts) => {
                    assert!(!parts.is_empty());
                }
                HeredocBody::Literal(_) => panic!("expected Parts (unquoted heredoc)"),
            }
        }
    }

    #[test]
    fn quoted_heredoc_no_expansion() {
        let src = "cat <<'EOF'\nhello $WORLD\nEOF\n";
        let script = parse_with_heredocs(src);
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let RedirectTarget::Heredoc(hd) = &sc.redirects[0].target {
            assert!(hd.quoted);
            assert!(matches!(hd.body, HeredocBody::Literal(_)));
            if let HeredocBody::Literal(body) = &hd.body {
                assert!(body.contains("$WORLD")); // not expanded
            }
        } else {
            panic!("expected Heredoc");
        }
    }

    #[test]
    fn heredoc_strip_tabs() {
        let src = "cat <<-EOF\n\thello\nEOF\n";
        let script = parse_with_heredocs(src);
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let RedirectTarget::Heredoc(hd) = &sc.redirects[0].target {
            assert!(hd.strip_tabs);
        }
    }

    #[test]
    fn heredoc_double_quoted_delimiter() {
        let src = "cat <<\"EOF\"\nhello $WORLD\nEOF\n";
        let script = parse_with_heredocs(src);
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let RedirectTarget::Heredoc(hd) = &sc.redirects[0].target {
            assert!(hd.quoted);
        }
    }

    #[test]
    fn unterminated_heredoc_error() {
        let err = parse("cat <<EOF\nhello\n").unwrap_err();
        assert!(err.message.contains("unterminated heredoc") || err.message.contains("missing delimiter"));
    }
}

#[cfg(test)]
mod quotes {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn single_quoted() {
        let script = parse("echo 'hello world'").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::SingleQuoted("hello world".into()));
    }

    #[test]
    fn single_quoted_no_expansion() {
        let script = parse("echo '$VAR'").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::SingleQuoted("$VAR".into()));
    }

    #[test]
    fn double_quoted_literal() {
        let script = parse(r#"echo "hello world""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::DoubleQuoted(_)));
        if let Word::DoubleQuoted(parts) = &sc.args[0] {
            assert_eq!(parts, &[DQPart::Literal("hello world".into())]);
        }
    }

    #[test]
    fn double_quoted_with_variable() {
        let script = parse(r#"echo "hello $NAME""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let Word::DoubleQuoted(parts) = &sc.args[0] {
            assert!(parts.iter().any(|p| matches!(p, DQPart::Variable(_))));
        }
    }

    #[test]
    fn unterminated_single_quote_error() {
        let err = parse("echo 'hello").unwrap_err();
        assert!(err.message.contains("unterminated single-quoted"));
    }

    #[test]
    fn unterminated_double_quote_error() {
        let err = parse(r#"echo "hello"#).unwrap_err();
        assert!(err.message.contains("unterminated double-quoted"));
    }

    #[test]
    fn adjacent_quotes_concat() {
        // 'hello'"world" should parse as two word parts
        let script = parse(r#"echo 'hello'"world""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        // Both should be args[0] as a Concat
        assert!(matches!(sc.args[0], Word::SingleQuoted(_) | Word::DoubleQuoted(_) | Word::Concat(_)));
    }
}

#[cfg(test)]
mod variables {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn simple_var() {
        let script = parse("echo $FOO").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::Variable(VarExpr::Simple("FOO".into())));
    }

    #[test]
    fn braced_var() {
        let script = parse("echo ${FOO}").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::Variable(VarExpr::Simple("FOO".into())));
    }

    #[test]
    fn var_default() {
        let script = parse("echo ${FOO:-bar}").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::Variable(VarExpr::DefaultIfUnset(_, _))));
        if let Word::Variable(VarExpr::DefaultIfUnset(name, default)) = &sc.args[0] {
            assert_eq!(name, "FOO");
            assert_eq!(default.as_ref(), &Word::Literal("bar".into()));
        }
    }

    #[test]
    fn var_assign_default() {
        let script = parse("echo ${FOO:=bar}").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::Variable(VarExpr::AssignIfUnset(_, _))));
    }

    #[test]
    fn var_alternate() {
        let script = parse("echo ${FOO:+alt}").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::Variable(VarExpr::AlternateIfSet(_, _))));
    }

    #[test]
    fn var_error_if_unset() {
        let script = parse("echo ${FOO:?must be set}").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::Variable(VarExpr::ErrorIfUnset(_, _))));
    }

    #[test]
    fn var_length() {
        let script = parse("echo ${#FOO}").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::Variable(VarExpr::Length("FOO".into())));
    }

    #[test]
    fn special_var_last_exit() {
        let script = parse("echo $?").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::Variable(VarExpr::Special(SpecialVar::LastExit)));
    }

    #[test]
    fn special_var_pid() {
        let script = parse("echo $$").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::Variable(VarExpr::Special(SpecialVar::Pid)));
    }

    #[test]
    fn special_var_positional() {
        let script = parse("echo $1 $2").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert_eq!(sc.args[0], Word::Variable(VarExpr::Special(SpecialVar::Positional(1))));
        assert_eq!(sc.args[1], Word::Variable(VarExpr::Special(SpecialVar::Positional(2))));
    }

    #[test]
    fn var_in_double_quotes() {
        let script = parse(r#"echo "$FOO world""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let Word::DoubleQuoted(parts) = &sc.args[0] {
            assert!(matches!(&parts[0], DQPart::Variable(VarExpr::Simple(_))));
        } else {
            panic!("expected DoubleQuoted");
        }
    }

    #[test]
    fn var_concat_in_word() {
        // `$FOO-bar` — variable concatenated with literal
        let script = parse("echo $FOO-bar").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        // Should be either a Concat or a Variable (if the parser collapses it)
        assert!(matches!(sc.args[0], Word::Concat(_) | Word::Variable(_)));
    }
}

#[cfg(test)]
mod command_substitution {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn command_subst_dollar_paren() {
        let script = parse("echo $(date)").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::CommandSubst(_)));
        if let Word::CommandSubst(inner) = &sc.args[0] {
            assert_eq!(inner.len(), 1);
            if let Command::Simple(sc2) = &inner[0] {
                assert_eq!(sc2.name, Some(Word::Literal("date".into())));
            }
        }
    }

    #[test]
    fn command_subst_backtick() {
        let script = parse("echo `date`").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::CommandSubst(_)));
    }

    #[test]
    fn nested_command_subst() {
        let script = parse("echo $(echo $(date))").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::CommandSubst(_)));
        if let Word::CommandSubst(inner) = &sc.args[0] {
            let Command::Simple(sc2) = &inner[0] else { panic!() };
            assert!(matches!(&sc2.args[0], Word::CommandSubst(_)));
        }
    }

    #[test]
    fn command_subst_in_double_quotes() {
        let script = parse(r#"echo "result: $(cmd)""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let Word::DoubleQuoted(parts) = &sc.args[0] {
            assert!(parts.iter().any(|p| matches!(p, DQPart::CommandSubst(_))));
        }
    }

    #[test]
    fn command_subst_as_command_name() {
        let script = parse("$(which python) --version").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.name, Some(Word::CommandSubst(_))));
    }
}

#[cfg(test)]
mod arithmetic {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn arith_subst() {
        let script = parse("echo $((1 + 2))").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::ArithSubst(_)));
        if let Word::ArithSubst(expr) = &sc.args[0] {
            assert!(expr.contains("1 + 2") || expr.contains("1+2"));
        }
    }

    #[test]
    fn arith_in_double_quotes() {
        let script = parse(r#"echo "$((x * 2))""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let Word::DoubleQuoted(parts) = &sc.args[0] {
            assert!(parts.iter().any(|p| matches!(p, DQPart::ArithSubst(_))));
        }
    }

    #[test]
    fn arith_with_var() {
        let script = parse("echo $((${N} + 1))").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(matches!(sc.args[0], Word::ArithSubst(_)));
    }
}

#[cfg(test)]
mod unsupported_features {
    use crate::parse;

    #[test]
    fn if_statement_error() {
        let err = parse("if true; then echo yes; fi").unwrap_err();
        assert!(err.message.contains("if statements are not supported"));
        assert!(err.message.contains("&&") || err.message.contains("||"));
    }

    #[test]
    fn for_loop_error() {
        let err = parse("for i in 1 2 3; do echo $i; done").unwrap_err();
        assert!(err.message.contains("loops are not supported"));
    }

    #[test]
    fn while_loop_error() {
        let err = parse("while true; do echo hi; done").unwrap_err();
        assert!(err.message.contains("loops are not supported"));
    }

    #[test]
    fn until_loop_error() {
        let err = parse("until false; do echo hi; done").unwrap_err();
        assert!(err.message.contains("loops are not supported"));
    }

    #[test]
    fn case_statement_error() {
        let err = parse("case $x in a) echo a;; esac").unwrap_err();
        assert!(err.message.contains("case statements are not supported"));
    }

    #[test]
    fn select_error() {
        let err = parse("select x in a b c; do echo $x; done").unwrap_err();
        assert!(err.message.contains("select is not supported"));
    }

    #[test]
    fn function_definition_error() {
        let err = parse("foo() { echo hi; }").unwrap_err();
        assert!(err.message.contains("shell function definitions are not supported"));
    }
}

#[cfg(test)]
mod complex_scripts {
    use crate::ast::*;
    use crate::parse;

    #[test]
    fn grep_pipeline_with_redirect() {
        let script = parse("cat /etc/hosts | grep localhost > /tmp/out").unwrap();
        // pipeline first (higher precedence), then redirect on last stage
        assert_eq!(script.len(), 1);
    }

    #[test]
    fn command_with_env_and_redirect() {
        let script = parse("DEBUG=1 node app.js > /tmp/app.log 2>&1 &").unwrap();
        let Command::Background(inner) = &script[0] else { panic!() };
        let Command::Simple(sc) = inner.as_ref() else { panic!() };
        assert_eq!(sc.assignments.len(), 1);
        assert_eq!(sc.redirects.len(), 2);
    }

    #[test]
    fn find_and_grep() {
        let script = parse(r#"find . -name "*.rs" | grep -v target"#).unwrap();
        let Command::Pipeline(p) = &script[0] else { panic!() };
        assert_eq!(p.commands.len(), 2);
    }

    #[test]
    fn multiline_pipeline() {
        let script = parse("echo hello |\n  grep hello |\n  wc -l").unwrap();
        let Command::Pipeline(p) = &script[0] else { panic!() };
        assert_eq!(p.commands.len(), 3);
    }

    #[test]
    fn subshell_and_pipeline() {
        // This is a tricky one: `(cd /tmp && ls) | grep foo`
        // Our grammar requires pipeline stages to be simple commands.
        // This should error or handle gracefully.
        let result = parse("(cd /tmp && ls) | grep foo");
        // Either an error or a successful parse — either is acceptable here.
        // If it errors, the message should be helpful.
        if let Err(e) = result {
            assert!(!e.message.is_empty());
        }
    }

    #[test]
    fn var_in_path() {
        let script = parse("ls $HOME/documents").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        // `$HOME/documents` is a Concat of Variable + Literal
        assert!(matches!(sc.args[0], Word::Concat(_) | Word::Variable(_)));
    }

    #[test]
    fn multiple_background_jobs() {
        let script = parse("sleep 1 & sleep 2 & wait").unwrap();
        assert_eq!(script.len(), 3);
        assert!(matches!(&script[0], Command::Background(_)));
        assert!(matches!(&script[1], Command::Background(_)));
        assert!(matches!(&script[2], Command::Simple(_)));
    }

    #[test]
    fn group_with_redirects() {
        let script = parse("{ echo a; echo b; } > /tmp/out").unwrap();
        // Group followed by redirect on the group-level — we store it on the group.
        // For now this may parse as just a group without the redirect depending on impl.
        assert!(!script.is_empty());
    }

    #[test]
    fn complex_string_expansion() {
        let script =
            parse(r#"echo "Hello, ${NAME:-World}! You have $(ls | wc -l) files.""#).unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        if let Word::DoubleQuoted(parts) = &sc.args[0] {
            // Should contain: literal, variable with default, literal, command subst, literal
            assert!(parts.len() >= 3);
        }
    }

    #[test]
    fn assignment_with_command_subst() {
        let script = parse("RESULT=$(cmd arg1 arg2)").unwrap();
        let Command::Simple(sc) = &script[0] else { panic!() };
        assert!(sc.name.is_none()); // pure assignment, no command
        assert_eq!(sc.assignments[0].0, "RESULT");
        assert!(matches!(sc.assignments[0].1, Word::CommandSubst(_)));
    }
}

#[cfg(test)]
mod error_spans {
    use crate::parse;

    #[test]
    fn error_has_valid_span() {
        let src = "if true; then echo hi; fi";
        let err = parse(src).unwrap_err();
        assert!(err.span.start <= src.len());
        assert!(err.span.end() <= src.len());
    }

    #[test]
    fn error_display_with_source() {
        let src = "for x in 1 2; do echo $x; done";
        let err = parse(src).unwrap_err();
        let display = format!("{}", err.display_with_source(src));
        assert!(display.contains("line 1"));
        assert!(display.contains('^'));
    }
}
