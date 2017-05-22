""" Extremely simple lisp parser, inspired by http://norvig.com/lispy2.html """

from __future__ import print_function

import cStringIO as StringIO
import operator as op
import os.path
import re
import subprocess
import sys


class Symbol(str):
    interned = {}

    @classmethod
    def intern(cls, name):
        sym = cls.interned.get(name, Symbol(name))
        cls.interned[name] = sym
        return sym

    def __repr__(self):
        return '#<%s>' % self


class Scope(dict):
    def __init__(self, params=(), args=(), parent=None, git=None):
        self.parent = parent
        self.git_scope = git

        if isinstance(params, Symbol):
            self.update({params: list(args)})

        else:
            assert len(params) == len(args)
            self.update(zip(params, args))

    def find(self, ident):
        if ident in self:
            return self[ident]

        elif self.parent is not None:
            return self.parent.find(ident)

        elif self.git_scope is not None:
            exp = self.git_scope.find(ident)
            return eval_exp(exp, self)

        raise LookupError(ident)


def make_global_scope(repo_path, fname):
    git = GitScope(repo_path, fname)
    global_scope = Scope(git=git)

    global_scope.update({
        '+': lambda *args: sum(args),
        '-': op.sub,
        '*': lambda *args: reduce(lambda a, b: a*b, args, 1),
        '/': op.div,
        'not': op.not_,
        '>': op.gt,
        '<': op.lt,
        '>=': op.ge,
        '<=': op.le,
        '=': op.eq,
        'list': lambda *args: list(args),
        'read': read,
        'eval': lambda exp: eval_exp(exp, global_scope)
    })

    return global_scope


class GitScope(Scope):
    def __init__(self, repo_path, fname='time.lisp'):
        self.repo = os.path.join(repo_path, '.git')
        self.file_name = fname

    def __repr__(self):
        return '#git{%s}' % self.file_name

    def find(self, ident):
        print('Entering time machine: %s' % ident)

        subprocess.check_call([
            'git', '--git-dir', self.repo, 'checkout', '-f', ident
        ])

        subprocess.check_call([
            'git', '--git-dir', self.repo, 'checkout', '--', self.file_name
        ])

        with open(self.file_name, 'r') as fp:
            tokens = tokenize(fp)
            exp = parse(tokens)

            return exp


class Lambda(object):
    def __init__(self, params, body, scope):
        self.params, self.body, self.scope = params, body, scope

    def __call__(self, *args):
        call_scope = Scope(self.params, args, self.scope)
        return eval_exp(self.body, call_scope)


EOF = Symbol('#<eof>')


def tokenize(buf):
    tokenizer = r'''^\s*([(')]|"(?:[^"])*"|;.*|[^\s('"`;)]+)(.*)'''

    line = ''

    while True:
        if line == '':
            line = buf.readline()

        if line == '':
            yield EOF

        match = re.match(tokenizer, line)
        if not match:
            yield EOF
            break

        token, line = match.groups()
        if token != '' and not token.startswith(';'):
            yield token


def atom(tok):
    if tok == '#t':
        return True
    elif tok == '#f':
        return False
    elif tok[0] == '"':
        return tok[1:-1].decode('string_escape')

    types = [int, float, Symbol.intern]
    for typ in types:
        try:
            return typ(tok)
        except ValueError:
            pass
    else:
        raise SyntaxError('everything is broken.')


def parse(tokens):
    def handle_tok(tok):
        if tok == '(':
            lst = []

            for tok in tokens:
                if tok == ')':
                    return lst
                elif tok == EOF:
                    raise SyntaxError('expected )')
                else:
                    lst.append(handle_tok(tok))

        elif tok == ')':
            raise SyntaxError('unmatched )')

        elif tok == "'":
            return [Symbol.intern('quote'), handle_tok(next(tokens))]

        elif tok == EOF:
            raise SyntaxError('unexpected EOF')

        else:
            return atom(tok)

    try:
        tok = next(tokens)
        return EOF if tok is EOF else handle_tok(tok)
    except StopIteration:
        raise SyntaxError('unexpected EOF')


def read(string):
    io = StringIO.StringIO(string)
    return parse(tokenize(io))


def eval_exp(exp, scope):
    if isinstance(exp, Symbol):
        return scope.find(exp)

    # atoms unevaluated
    elif not isinstance(exp, list):
        return exp

    # evaluate function call
    fn_atom, args = exp[0], exp[1:]

    # builtin forms
    if fn_atom is Symbol.intern('if'):
        (cond, then, else_) = args
        branch = then if eval_exp(cond, scope) else else_

        return eval_exp(branch, scope)

    elif fn_atom is Symbol.intern('lambda'):
        (params, body) = args
        return Lambda(params, body, scope)

    elif fn_atom is Symbol.intern('quote'):
        if len(args) == 1:
            return args[0]

        return args

    elif fn_atom is Symbol.intern('do'):
        val = None
        for arg in args:
            val = eval_exp(arg, scope)
        return val

    elif fn_atom is Symbol.intern('print'):
        vals = [eval_exp(arg, scope) for arg in args]
        print(repr(vals))

        return vals

    elif fn_atom is Symbol.intern('stash!'):
        # TODO: this should modify the file and then git stash
        # TODO: (stash! (lambda (x) (x + 1)))
        pass

    elif fn_atom is Symbol.intern('pop!'):
        # TODO: this should pop the stash and return the parsed AST
        # TODO: (eval (pop!))
        pass

    elif fn_atom is Symbol.intern('commit!'):
        # TODO: this should apply a commit on top of the named branch.
        # TODO: (commit! "fn/add-1" (lambda (x) (x + 1)))
        pass

    else:
        exps = [eval_exp(e, scope) for e in exp]
        fn, args = exps[0], exps[1:]

        if isinstance(fn, Lambda):
            scope = Scope(fn.params, args, fn.scope)
            return fn(*args)
        else:
            return fn(*args)
