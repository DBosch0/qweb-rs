"""qweb_rs: QWeb templating engine implemented in Rust.

``render(template_name, values, load, **options)`` is a drop-in replacement
for ``odoo.addons.base.models.ir_qweb.render`` (database-less rendering).
"""
import logging

from markupsafe import Markup

from . import _helpers
from ._helpers import Bridge, LoadBridge
from ._qweb_rs import (
    FLAG_COMPILE_HOOKS,
    FLAG_POST_PROCESSING,
    FLAG_WITH_CONTEXT,
    Attrib,
    CompiledTemplate,
    Compiler,
    Element,
    Engine,
    QwebContent,
    apply_inheritance,
)

__all__ = [
    'FLAG_COMPILE_HOOKS', 'FLAG_POST_PROCESSING', 'FLAG_WITH_CONTEXT', 'Attrib', 'Bridge', 'CompiledTemplate',
    'Compiler', 'Element', 'Engine', 'LoadBridge', 'QWebError', 'QwebContent', 'apply_inheritance', 'render',
]

_logger = logging.getLogger('odoo.addons.base.models.ir_qweb')

try:
    from odoo.addons.base.models.ir_qweb import QWebError, QWebErrorInfo
    from odoo.exceptions import UserError
except Exception:  # noqa: BLE001
    class UserError(Exception):  # noqa: N818
        pass

    class QWebErrorInfo:
        def __init__(self, error, ref_name, ref, path, element, source, surrounding):
            self.error = error
            self.template = ref_name
            self.ref = ref
            self.path = path
            self.element = element
            self.source = source
            self.surrounding = surrounding

        def __str__(self):
            info = [self.error]
            if self.template is not None:
                info.append(f'Template: {self.template}')
            if self.ref is not None:
                info.append(f'Reference: {self.ref}')
            if self.path is not None:
                info.append(f'Path: {self.path}')
            if self.element is not None:
                info.append(f'Element: {self.element}')
            return '\n'.join(info)

    class QWebError(Exception):
        def __init__(self, qweb):
            super().__init__(qweb)
            self.qweb = qweb

        def __str__(self):
            return f"Error while rendering the template:\n{self.qweb}"


_engine = None


def _default_engine():
    global _engine  # noqa: PLW0603
    if _engine is None:
        _engine = Engine()
    return _engine


def wrap_error(error, template):
    """Wrap an exception into a ``QWebError`` like ``IrQweb._render_iterall``."""
    if isinstance(error, (QWebError, UserError)) or hasattr(error, 'qweb'):
        return error
    ref = getattr(error, 'qweb_template', None) or template
    info = QWebErrorInfo(
        f'{error.__class__.__name__}: {error}', ref, ref,
        getattr(error, 'qweb_path', None), getattr(error, 'qweb_element', None), [], None,
    )
    wrapped = QWebError(info)
    wrapped.__cause__ = error
    return wrapped


def render(template_name, values, load, **options):
    """Render a template without database (like ``ir_qweb.render``).

    :param template_name: template identifier
    :param values: template values to be used for rendering
    :param load: function like ``load(template_name)`` which returns a tuple
        ``(etree, xmlid)`` from the given template name
    :param options: used to compile the template
    :returns: ``Markup``
    """
    values = values.copy() if values else {}
    if '0' in values:
        _logger.warning('values[0] should be unset when call the _render method and only set into the template.')
        values.pop('0')
    values['true'] = True
    values['false'] = False
    _helpers.check_values(values)
    options = {'minimal_qcontext': True, **options}
    try:
        return Markup(_default_engine().render(template_name, values, load, options))
    except Exception as e:  # noqa: BLE001
        wrapped = wrap_error(e, template_name)
        if wrapped is e:
            raise
        raise wrapped from e
