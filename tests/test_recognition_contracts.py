"""Contract tests for Chinese built-in recognition and persisted custom rules."""
import app.main as main


def test_builtin_chinese_account_patterns_and_ip_boundary():
    text = "微信 wxid_ab12345，QQ号 12345678，邮编 100000，护照 E12345678，MAC  AA:BB:CC:DD:EE:FF，IP 1.2.3.4.5"
    entities = main.analyze(text)
    kinds = {item["type"] for item in entities}
    assert {"WECHAT_ID", "QQ_NUMBER", "POSTAL_CODE", "MAC_ADDRESS"} <= kinds
    assert not any(item["type"] == "IP_ADDRESS" and item["text"] == "1.2.3.4.5" for item in entities)


def test_custom_rule_enabled_flag_is_honored_and_persisted():
    rule_id = "fixture-rule"
    c = main.conn()
    c.execute("DELETE FROM rules WHERE id=?", (rule_id,))
    c.execute("INSERT INTO rules VALUES(?,?,?,?,?,?,?)", (rule_id, "测试规则", "word", "机密项目", "PROJECT", "项目", 0))
    c.commit(); c.close()
    try:
        assert not any(e["type"] == "PROJECT" for e in main.analyze("机密项目"))
        c = main.conn(); c.execute("UPDATE rules SET enabled=1 WHERE id=?", (rule_id,)); c.commit(); c.close()
        assert any(e["type"] == "PROJECT" for e in main.analyze("机密项目"))
    finally:
        c = main.conn(); c.execute("DELETE FROM rules WHERE id=?", (rule_id,)); c.commit(); c.close()


def test_ner_low_confidence_candidate_defaults_unselected(monkeypatch):
    class Stub:
        def analyze(self, text):
            return [{"type": "PERSON", "text": "张三", "start": 0, "end": 2, "score": 0.2, "source": "ner"}]
    monkeypatch.setattr(main, "ner_service", Stub())
    entities = main.analyze("张三")
    ner = next(e for e in entities if e["type"] == "PERSON")
    assert ner["selected"] is False
