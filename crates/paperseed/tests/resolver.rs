use paperseed::resolver::parse_arxiv_atom;

#[test]
fn parses_minimal_arxiv_atom() {
    let atom = r#"
    <feed>
      <entry>
        <id>http://arxiv.org/abs/1234.5678v1</id>
        <published>2024-01-02T00:00:00Z</published>
        <title> A Useful Open Paper </title>
        <author><name>Ada Lovelace</name></author>
      </entry>
    </feed>
    "#;
    let results = parse_arxiv_atom(atom);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source, "arxiv");
    assert_eq!(results[0].title, "A Useful Open Paper");
    assert_eq!(results[0].authors, vec!["Ada Lovelace"]);
    assert_eq!(results[0].year, Some(2024));
}
