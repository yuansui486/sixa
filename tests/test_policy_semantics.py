from __future__ import annotations

import pytest

from app.policies import DEFAULT_POLICIES, replacement_for


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("北京某科技有限公司", "某公司"),
        ("北京大学", "某学校"),
        ("市第一医院", "某医院"),
        ("中国某银行", "某金融机构"),
        ("行业协会", "某机构"),
    ],
)
def test_organization_replacement_uses_natural_context(value: str, expected: str) -> None:
    assert replacement_for({"type": "ORGANIZATION", "text": value}, None, {}) == expected


@pytest.mark.parametrize(
    ("entity_type", "value", "expected"),
    [
        ("PHONE_NUMBER", "13800138000", "138****8000"),
        ("ID_CARD", "110101199001011234", "110101********1234"),
        ("BANK_CARD", "6222021234567890", "************7890"),
        ("QQ_NUMBER", "12345678", "12****78"),
        ("POSTAL_CODE", "100000", "10**00"),
    ],
)
def test_structured_numbers_keep_only_non_sensitive_fragments(
    entity_type: str, value: str, expected: str
) -> None:
    assert replacement_for({"type": entity_type, "text": value}, None, {}) == expected


def test_unknown_entity_never_exposes_internal_mask_token() -> None:
    result = replacement_for({"type": "UNKNOWN_KIND", "text": "秘密"}, None, {})
    assert result == "已脱敏"
    assert "???" not in result
    assert "__MASKED_" not in result


@pytest.mark.parametrize(
    ("entity_type", "value", "expected"),
    [
        ("EMAIL", "user@example.com", "***@***"),
        ("EMAIL_ADDRESS", "user@example.com", "***@***"),
        ("IP_ADDRESS", "192.168.10.23", "***.***.***.***"),
    ],
)
def test_network_identifiers_use_stable_structured_placeholders(
    entity_type: str, value: str, expected: str
) -> None:
    assert replacement_for({"type": entity_type, "text": value}, None, {}) == expected


def test_user_replacement_overrides_semantic_default() -> None:
    policies = {
        key: dict(value) for key, value in DEFAULT_POLICIES.items()
    }
    policies["ORGANIZATION"]["replacement"] = "合作单位"
    assert replacement_for(
        {"type": "ORGANIZATION", "text": "北京某科技有限公司"}, policies, {}
    ) == "合作单位"


def test_all_builtin_image_policies_default_to_blur() -> None:
    assert all(policy["image_action"] == "blur" for policy in DEFAULT_POLICIES.values())
