"""Conservative SPDX license-expression grammar and license elections."""

import re

OPERATORS = {"AND", "OR", "WITH", "(", ")"}


class ExpressionError(ValueError):
    """An expression or election is malformed or not offered."""


def alternatives(expression):
    """Return alternatives as sets of required terms; AND binds tighter than OR."""
    tokens = re.findall(r"[A-Za-z0-9.+-]+|[()]", expression or "")
    if not tokens or "".join(tokens) != re.sub(r"\s+", "", expression):
        raise ExpressionError("Invalid license expression")
    position = 0

    def peek(*expected):
        return position < len(tokens) and tokens[position] in expected

    def primary():
        nonlocal position
        if position >= len(tokens) or tokens[position] in OPERATORS - {"("}:
            raise ExpressionError("Missing or unexpected expression term")
        token, position = tokens[position], position + 1
        if token == "(":
            options = disjunction()
            if not peek(")"):
                raise ExpressionError("Unbalanced expression")
            position += 1
            return options
        if peek("WITH"):
            if position + 1 >= len(tokens) or tokens[position + 1] in OPERATORS:
                raise ExpressionError("Missing exception")
            token, position = f"{token} WITH {tokens[position + 1]}", position + 2
        return {frozenset([token])}

    def conjunction():
        nonlocal position
        options = primary()
        while peek("AND"):
            position += 1
            terms = primary()
            options = {left | right for left in options for right in terms}
        return options

    def disjunction():
        nonlocal position
        options = conjunction()
        while peek("OR"):
            position += 1
            options |= conjunction()
        return options

    options = disjunction()
    if position != len(tokens):
        raise ExpressionError("Trailing expression terms")
    return options


def legacy_alternatives(expression):
    """Accept the legacy Cargo `A/B` spelling only as a plain list of alternatives."""
    compact = re.sub(r"\s+", "", expression or "")
    if re.fullmatch(r"[A-Za-z0-9.+-]+(?:/[A-Za-z0-9.+-]+)+", compact):
        expression = " OR ".join(compact.split("/"))
    return alternatives(expression)


def election(expression, elected):
    """Return the elected alternative's terms and every term the expression offers."""
    options, chosen = legacy_alternatives(expression), alternatives(elected)
    if len(chosen) != 1 or not chosen <= options:
        raise ExpressionError("Elected license is not offered by the package expression")
    return next(iter(chosen)), set().union(*options)
