import pglast
from pglast.enums import AlterTableType, ConstrType

from .. import SquabbleException


known_rules = {}


def register_rule(cls):
    meta = cls.meta()
    print('registering rule: %s' % repr(meta))

    known_rules[meta['name']] = {
        'class': cls,
        'meta': meta
    }


class Rule:
    def __init__(self, options):
        self._options = options

    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        register_rule(cls)

    @classmethod
    def meta(cls):
        split_doc = (cls.__doc__ or '').strip().split('\n', 1)

        return {
            'name': cls.__name__,
            'description': split_doc[0],
            'help': split_doc[1] if len(split_doc) == 2 else None
        }

    def enable(self, ctx):
        raise NotImplementedError('must be overridden by subclass')


class RuleConfigurationException(SquabbleException):
    pass


class AddColumnDisallowConstraints(Rule):
    """
    Prevent adding a column with certain constraints to an existing table

    Configuration:

        "rules": {
            "AddColumnDisallowConstraints": {
                "disallowed": ["DEFAULT", "FOREIGN"]
            }
        }

    Valid constraint types:
      - DEFAULT
      - NULL
      - NOT NULL
      - FOREIGN
      - UNIQUE
    """

    CONSTRAINT_MAP = {
        'DEFAULT': ConstrType.CONSTR_DEFAULT,
        'NULL': ConstrType.CONSTR_NULL,
        'NOT NULL': ConstrType.CONSTR_NOTNULL,
        'FOREIGN': ConstrType.FOREIGN,
        'UNIQUE': ConstrType.UNIQUE,
    }

    MESSAGES = {
        'constraint_not_allowed': 'column {col} has a disallowed constraint'
    }

    def __init__(self, opts):
        super().__init__(self, opts)

        if 'disallowed' not in opts or opts['disallowed'] == []:
            raise RuleConfigurationException(
                'must specify `disallowed` constraints')

        constraints = []

        for c in opts['disallowed']:
            ty = self.CONSTRAINT_MAP[c.upper()]
            if ty is None:
                raise RuleConfigurationException('unknown constraint: `%s`' % c)

            constraints.append(ty)

        self._blocked_constraints = set(constraints)

    def enable(self, ctx):
        ctx.register(['AlterTableCmd'], lambda c, n: self.check(c, n))

    def check(self, ctx, node):
        """
        Node is an `AlterTableCmd`:

        {
          'AlterTableCmd': {
            'def': {
              'ColumnDef': {
                'colname': 'bar',
                'constraints': [{'Constraint': {'contype': 2, 'location': 35}}]
              }
            }
          }
        }
        """

        # We only care about adding a column
        if node.subtype != AlterTableType.AT_AddColumn:
            return

        constraints = node['def'].constraints

        # No constraints imposed, nothing to do.
        if constraints == pglast.Missing:
            return

        for constraint in constraints:
            if constraint.contype in self._blocked_constraints:
                ctx.fail('constraint_not_allowed', node=constraint, msg={
                    'col': node['def'].colname
                })


def load():
    return [
        AddColumnDisallowConstraints
    ]
