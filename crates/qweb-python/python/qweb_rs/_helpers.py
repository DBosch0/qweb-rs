"""Python-side helpers used by the Rust extension.

* ``compile_opaque``: compiles the expressions the native evaluator does not
  handle (lambda, comprehensions, f-strings, ``*args``...). The name
  rewriting reproduces ``IrQweb._compile_expr_tokens`` token for token, so
  these expressions behave exactly as in the Python engine (quirks included).
* ``load_template``: calls a user ``load`` function and serializes the tree.
"""

import functools
import io
import logging
import token
import tokenize
import types

_logger = logging.getLogger("odoo.addons.base.models.ir_qweb")

try:  # Odoo available: use its exact sandbox definitions.
    from odoo.tools.safe_eval import _BUILTINS, assert_valid_codeobj
    from odoo.addons.base.models.ir_qweb import _SAFE_QWEB_OPCODES

    HAS_ODOO = True
except Exception:  # noqa: BLE001
    HAS_ODOO = False
    _BUILTINS = {
        "True": True,
        "False": False,
        "None": None,
        "bytes": bytes,
        "str": str,
        "unicode": str,
        "bool": bool,
        "int": int,
        "float": float,
        "enumerate": enumerate,
        "dict": dict,
        "list": list,
        "tuple": tuple,
        "map": map,
        "abs": abs,
        "min": min,
        "max": max,
        "sum": sum,
        "reduce": functools.reduce,
        "filter": filter,
        "sorted": sorted,
        "round": round,
        "len": len,
        "repr": repr,
        "set": set,
        "all": all,
        "any": any,
        "ord": ord,
        "chr": chr,
        "divmod": divmod,
        "isinstance": isinstance,
        "range": range,
        "xrange": range,
        "zip": zip,
        "Exception": Exception,
    }

BUILTINS = {k: v for k, v in _BUILTINS.items() if not k.startswith("_")}

ALLOWED_KEYWORD = frozenset(
    [
        "False",
        "None",
        "True",
        "and",
        "as",
        "elif",
        "else",
        "for",
        "if",
        "in",
        "is",
        "not",
        "or",
    ]
    + list(_BUILTINS)
)

# Globals of the evaluated expressions (like ``IrQweb.__prepare_globals``).
GLOBALS = dict(_BUILTINS)
GLOBALS["__builtins__"] = GLOBALS

QWEB = token.NT_OFFSET - 1


def _compile_expr_tokens(
    tokens, allowed_keys, argument_names=None, raise_on_missing=False
):
    """Port of ``IrQweb._compile_expr_tokens``."""
    open_bracket_index = -1
    bracket_depth = 0

    argument_name = "_arg_%s__"
    argument_names = argument_names or []

    for index, t in enumerate(tokens):
        if t.exact_type in [token.LPAR, token.LSQB, token.LBRACE]:
            bracket_depth += 1
        elif t.exact_type in [token.RPAR, token.RSQB, token.RBRACE]:
            bracket_depth -= 1
        elif bracket_depth == 0 and t.exact_type == token.NAME:
            string = t.string
            if string == "lambda":
                for i in range(index + 1, len(tokens)):
                    t = tokens[i]
                    if t.exact_type == token.NAME:
                        argument_names.append(t.string)
                    elif t.exact_type == token.COMMA:
                        pass
                    elif t.exact_type == token.COLON:
                        break
                    elif t.exact_type == token.EQUAL:
                        raise NotImplementedError(
                            "Lambda default values are not supported"
                        )
                    else:
                        raise NotImplementedError(
                            "This lambda code style is not implemented."
                        )
            elif string == "for":
                for i in range(index + 1, len(tokens)):
                    t = tokens[i]
                    if t.exact_type == token.NAME:
                        if t.string == "in":
                            break
                        argument_names.append(t.string)
                    elif t.exact_type in [token.COMMA, token.LPAR, token.RPAR]:
                        pass
                    else:
                        raise NotImplementedError(
                            "This loop code style is not implemented."
                        )

    index = 0
    open_bracket_index = -1
    bracket_depth = 0

    while index < len(tokens):
        t = tokens[index]

        if t.exact_type in [token.LPAR, token.LSQB, token.LBRACE]:
            if bracket_depth == 0:
                open_bracket_index = index
            bracket_depth += 1
        elif t.exact_type in [token.RPAR, token.RSQB, token.RBRACE]:
            bracket_depth -= 1
            if bracket_depth == 0:
                code = _compile_expr_tokens(
                    tokens[open_bracket_index + 1 : index],
                    list(allowed_keys),
                    list(argument_names),
                    raise_on_missing,
                )
                code = tokens[open_bracket_index].string + code + t.string
                tokens[open_bracket_index : index + 1] = [
                    tokenize.TokenInfo(
                        QWEB, code, tokens[open_bracket_index].start, t.end, ""
                    )
                ]
                index = open_bracket_index

        index += 1

    code = []
    index = 0
    pos = tokens and tokens[0].start
    while index < len(tokens):
        t = tokens[index]
        string = t.string

        if t.start[0] != pos[0]:
            pos = (t.start[0], 0)
        space = t.start[1] - pos[1]
        if space:
            code.append(" " * space)
        pos = t.start

        if t.exact_type == token.NAME:
            if "__" in string:
                raise SyntaxError(
                    f"Using variable names with '__' is not allowed: {string!r}"
                )
            if string == "lambda":
                code.append("lambda ")
                index += 1
                while index < len(tokens):
                    t = tokens[index]
                    if t.exact_type == token.NAME and t.string in argument_names:
                        code.append(argument_name % t.string)
                    if t.exact_type in [token.COMMA, token.COLON]:
                        code.append(t.string)
                    if t.exact_type == token.COLON:
                        break
                    index += 1
                if t.end[0] != pos[0]:
                    pos = (t.end[0], 0)
                else:
                    pos = t.end
            elif string in argument_names:
                code.append(argument_name % t.string)
            elif (
                string in allowed_keys
                or (
                    index + 1 < len(tokens)
                    and tokens[index + 1].exact_type == token.EQUAL
                )
                or (
                    index > 0
                    and tokens[index - 1]
                    and tokens[index - 1].exact_type == token.DOT
                )
            ):
                code.append(string)
            elif (
                raise_on_missing
                or index + 1 < len(tokens)
                and tokens[index + 1].exact_type
                in [
                    token.DOT,
                    token.LPAR,
                    token.LSQB,
                    QWEB,
                ]
            ):
                code.append(f"values[{string!r}]")
            else:
                code.append(f"values.get({string!r})")
        elif t.type not in [tokenize.ENCODING, token.ENDMARKER, token.DEDENT]:
            code.append(string)

        if t.end[0] != pos[0]:
            pos = (t.end[0], 0)
        else:
            pos = t.end

        index += 1

    return "".join(code)


def rewrite_expr(expr, raise_on_missing=False):
    """Return the Python source of ``expr`` with namespaced values (``IrQweb._compile_expr``)."""
    readable = io.BytesIO(f"({expr or ''})".encode())
    try:
        tokens = list(tokenize.tokenize(readable.readline))
    except tokenize.TokenError:
        raise ValueError(f"Can not compile expression: {expr}")
    expression = _compile_expr_tokens(
        tokens, ALLOWED_KEYWORD, raise_on_missing=raise_on_missing
    )
    if HAS_ODOO:
        assert_valid_codeobj(
            _SAFE_QWEB_OPCODES, compile(expression, "<>", "eval"), expr
        )
    else:
        compile(expression, "<>", "eval")
    return f"({expression})"


def compile_opaque(expr, raise_on_missing=False):
    """Compile ``expr`` into a function ``f(values)``."""
    expression = rewrite_expr(expr, raise_on_missing)
    code = compile(f"lambda values: {expression}", f"<qweb: {expr[:60]}>", "eval")
    return eval(code, GLOBALS)


def load_template(load, name):
    """Call ``load(name) -> (etree | str, ref)`` and return the XML source."""
    tree, _ref = load(name)
    if isinstance(tree, str):
        return tree
    from lxml import etree

    return etree.tostring(tree, encoding="unicode", with_tail=False)


def warn(message):
    """Log a compile warning on the `ir_qweb` logger, like the Python engine."""
    _logger.warning(message)


def check_values(values):
    """``safe_eval.check_values``: refuse module objects."""
    if HAS_ODOO:
        from odoo.tools import safe_eval

        return safe_eval.check_values(values)
    for value in values.values():
        if isinstance(value, types.ModuleType):
            raise TypeError(f"Module {value} can not be used in evaluation context.")
    return values


class Bridge:
    """Base class of the objects connecting the engine to its environment.

    The engine calls these methods while compiling and rendering. ``flags``
    (``FLAG_*`` of ``qweb_rs``) enables the optional ones:

    * ``FLAG_WITH_CONTEXT``: ``with_context(context)`` returns the bridge used
      for a ``t-call`` with options (``t-options``, ``t-lang``, namespaces);
    * ``FLAG_POST_PROCESSING``: ``post_processing_att(tag, attrs)`` filters the
      dynamic attributes (and static ones when enabled by ``compile_hooks``);
    * ``FLAG_COMPILE_HOOKS``: ``compile_hooks()`` returns the hooks to use for a
      compilation: a dict with ``order`` (directive evaluation order),
      ``check_directive``, ``check_expr``, ``pre_processing`` and
      ``post_processing`` (booleans), or ``None``.
    """

    flags = 0

    def load(self, ref):
        """Return the template ``ref``: a ``CompiledTemplate``, its XML source,
        or a ``(source, viewid, xmlid)`` tuple."""
        raise NotImplementedError

    def has_groups(self, groups):
        raise AttributeError("'MockEnv' object has no attribute 'user'")

    def get_field(self, record, field_name, expression, tag, options, values):
        raise NotImplementedError(
            "Fields are not allowed in this rendering mode. Please use \"env['ir.qweb']._render\" method"
        )

    def get_widget(self, value, expression, tag, options, values):
        raise NotImplementedError(
            "Widgets are not allowed in this rendering mode. Please use \"env['ir.qweb']._render\" method"
        )

    def get_asset_nodes(
        self,
        bundle,
        css=True,
        js=True,
        binary=False,
        debug=False,
        defer_load=False,
        lazy_load=False,
        media=None,
        autoprefix=False,
    ):
        raise NotImplementedError(
            "Assets are not allowed in this rendering mode. Please use \"env['ir.qweb']._render\" method"
        )

    def with_context(self, context):
        return self

    def post_processing_att(self, tag, attrs):
        return attrs

    def compile_hooks(self):
        return None

    def has_directive(self, name):
        return False

    def compile_directive(self, name, el, compiler):
        raise TypeError(f"no handler for the directive 't-{name}'")

    def check_directive(self, name, el):
        pass

    def check_expr(self, expr):
        pass

    def pre_processing_att(self, attrs):
        for key in list(attrs):
            if key.endswith(".translate"):
                attrs[key.removesuffix(".translate")] = attrs.pop(key)

    def run_hook(self, hook, values):
        func, args = hook
        return func(values, *args)


class LoadBridge(Bridge):
    """Bridge of the database-less ``render``: templates come from ``load``."""

    __slots__ = ("_load",)

    def __init__(self, load):
        self._load = load

    def load(self, ref):
        if self._load is None:
            raise NotImplementedError("No template loader")
        return load_template(self._load, ref)
