from __future__ import annotations

import hashlib
from typing import Any

from presidio_analyzer import RecognizerResult
from presidio_anonymizer import AnonymizerEngine
from presidio_anonymizer.entities import OperatorConfig


_ANONYMIZER = AnonymizerEngine()

POLICY_VERSION = 2
_COMMON_IMAGE = {"image_action": "blur", "show_replacement": True, "blur_radius": 16, "pixel_size": 12, "color": "#000000"}
DEFAULT_POLICIES: dict[str, dict[str, Any]] = {
    "PERSON": {"text_action": "replace", "replacement": "某人", "image_action": "solid", "color": "#000000"},
    "ORGANIZATION": {"text_action": "replace", "replacement": "某机构", "image_action": "solid", "color": "#000000"},
    "LOCATION": {"text_action": "replace", "replacement": "某地点", "image_action": "solid", "color": "#000000"},
    "GPE": {"text_action": "replace", "replacement": "某地点", "image_action": "solid", "color": "#000000"},
    "ADDRESS": {"text_action": "replace", "replacement": "某地址", "image_action": "solid", "color": "#000000"},
    "PHONE": {"text_action": "mask", "replacement": "", "image_action": "solid", "color": "#000000"},
    "PHONE_NUMBER": {"text_action": "mask", "replacement": "", "image_action": "solid", "color": "#000000"},
    "ID_CARD": {"text_action": "mask", "replacement": "", "image_action": "solid", "color": "#000000"},
    "BANK_CARD": {"text_action": "mask", "replacement": "", "image_action": "solid", "color": "#000000"},
    "EMAIL": {"text_action": "replace", "replacement": "***@***", "image_action": "solid", "color": "#000000"},
    "EMAIL_ADDRESS": {"text_action": "replace", "replacement": "***@***", "image_action": "solid", "color": "#000000"},
    "IP_ADDRESS": {"text_action": "replace", "replacement": "0.0.0.0", "image_action": "solid", "color": "#000000"},
    "DATE_TIME": {"text_action": "replace", "replacement": "某日期", "image_action": "blur", "color": "#000000"},
    "MONEY": {"text_action": "replace", "replacement": "某金额", "image_action": "solid", "color": "#000000"},
    "LICENSE_PLATE": {"text_action": "replace", "replacement": "车牌号", "image_action": "solid", "color": "#000000"},
    "PASSPORT": {"text_action": "replace", "replacement": "证件号", "image_action": "solid", "color": "#000000"},
    "URL": {"text_action": "replace", "replacement": "***", "image_action": "solid", "color": "#000000"},
    "MAC_ADDRESS": {"text_action": "replace", "replacement": "***", "image_action": "solid", "color": "#000000"},
    "WECHAT_ID": {"text_action": "replace", "replacement": "微信用户", "image_action": "solid", "color": "#000000"},
    "QQ_NUMBER": {"text_action": "mask", "replacement": "", "image_action": "solid", "color": "#000000"},
    "POSTAL_CODE": {"text_action": "mask", "replacement": "", "image_action": "solid", "color": "#000000"},
    "CUSTOM": {"text_action": "replace", "replacement": "已脱敏", "image_action": "solid", "color": "#000000"},
    "DEFAULT": {"text_action": "token", "replacement": "", "image_action": "solid", "color": "#000000"},
}
# Normalize built-ins so every entity has the same usable image defaults.
for _name, _policy in DEFAULT_POLICIES.items():
    for _key, _value in _COMMON_IMAGE.items():
        _policy[_key] = _value


def policy_for(entity_type: str, policies: dict | None = None) -> dict[str, Any]:
    merged = dict(DEFAULT_POLICIES.get(entity_type, DEFAULT_POLICIES["DEFAULT"]))
    # A request-level DEFAULT is the common policy for every entity/region;
    # an entity-specific entry can then override individual fields.
    if policies and isinstance(policies.get("DEFAULT"), dict):
        merged.update(policies["DEFAULT"])
    if policies and entity_type != "DEFAULT" and isinstance(policies.get(entity_type), dict):
        merged.update(policies[entity_type])
    return merged


def _mask_value(value: str, entity_type: str) -> str:
    if entity_type in {"PHONE", "PHONE_NUMBER"} and len(value) >= 7:
        return value[:3] + "*" * max(1, len(value) - 7) + value[-4:]
    if entity_type == "QQ_NUMBER" and len(value) >= 5:
        return value[:2] + "*" * max(1, len(value) - 4) + value[-2:]
    if entity_type == "POSTAL_CODE" and len(value) == 6:
        return value[:2] + "**" + value[-2:]
    if entity_type == "ID_CARD" and len(value) >= 10:
        return value[:6] + "*" * (len(value) - 10) + value[-4:]
    if entity_type == "BANK_CARD" and len(value) >= 4:
        return "*" * (len(value) - 4) + value[-4:]
    return "*" * max(3, len(value))


def replacement_for(entity: dict, policies: dict | None, mapping: dict[tuple[str, str], str]) -> str:
    entity_type = str(entity.get("type", "DEFAULT"))
    original = str(entity.get("text", ""))
    policy = policy_for(entity_type, policies)
    action = policy.get("text_action", "token")
    key = (entity_type, original)
    if key in mapping:
        return mapping[key]
    # A custom word/regex recognizer may carry its own replacement from the
    # rule editor.  Treat an explicitly configured type policy as an override;
    # otherwise preserve the rule's replacement across every file adapter.
    custom_replacement = entity.get("replacement")
    # A literal custom rule carries its own replacement.  This must take
    # precedence over the generic CUSTOM policy so each user-defined word can
    # be configured independently.
    if entity.get("source") == "custom" and custom_replacement is not None and entity.get("inherit_policy") is not True:
        result = str(custom_replacement)
    elif action == "keep":
        result = original
    elif action == "replace":
        result = str(policy.get("replacement") or "***")
    elif action == "mask":
        result = _mask_value(original, entity_type)
    elif action == "hash":
        result = hashlib.sha256(original.encode("utf-8")).hexdigest()[:12]
    elif action == "pseudonym":
        result = {"PERSON": "某人", "ORGANIZATION": "某机构", "LOCATION": "某地点"}.get(entity_type, "已脱敏")
    else:
        digest = hashlib.sha256((entity_type + "\0" + original).encode("utf-8")).hexdigest()[:8].upper()
        result = f"__MASKED_{entity_type}_{digest}__"
    mapping[key] = result
    return result


def apply_entities(text: str, entities: list[dict], policies: dict | None = None, mapping: dict | None = None) -> tuple[str, dict]:
    mapping = mapping if mapping is not None else {}
    selected = [e for e in entities if e.get("selected", True)]
    ordered = sorted(selected, key=lambda item: (int(item["start"]), int(item["end"])))
    previous_end = -1
    for entity in ordered:
        start, end = int(entity["start"]), int(entity["end"])
        if start < 0 or end < start or end > len(text) or text[start:end] != entity.get("text"):
            raise ValueError("实体区间与原文不一致")
        if end <= start:
            raise ValueError("实体区间无效")
        if start < previous_end:
            raise ValueError("实体区间存在重叠")
        previous_end = end
    if not ordered:
        return text, mapping

    replacements: dict[tuple[str, str], str] = {}
    analyzer_results: list[RecognizerResult] = []
    for entity in ordered:
        entity_type = str(entity.get("type", "DEFAULT"))
        original = str(entity.get("text", ""))
        replacements[(entity_type, original)] = replacement_for(entity, policies, mapping)
        analyzer_results.append(
            RecognizerResult(
                entity_type=entity_type,
                start=int(entity["start"]),
                end=int(entity["end"]),
                score=float(entity.get("score", 1.0)),
            )
        )

    def operator_for(entity_type: str) -> OperatorConfig:
        return OperatorConfig(
            "custom",
            {"lambda": lambda value: replacements[(entity_type, value)]},
        )

    operators = {result.entity_type: operator_for(result.entity_type) for result in analyzer_results}
    result = _ANONYMIZER.anonymize(
        text=text,
        analyzer_results=analyzer_results,
        operators=operators,
        merge_entities_with_spaces=False,
    )
    return result.text, mapping
