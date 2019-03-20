use std::fmt;
use std::rc::Rc;
use std::cell::{RefCell};
use super::position;
use super::token::Token;
use super::scanner;
use super::errors;
use super::scope::*;
use super::ast::*;
use super::ast_objects::*;

pub struct Parser<'a> {
    objects: Objects,
    scanner: scanner::Scanner<'a>,
    errors: Rc<RefCell<errors::ErrorList>>,

    trace: bool,
    indent: isize,

    pos: position::Pos,
    token: Token,

    sync_pos: position::Pos,
    sync_count: isize,

    expr_level: isize,
    in_rhs: bool,

    pkg_scope: Option<ScopeIndex>,
    top_scope: Option<ScopeIndex>,
    unresolved: Vec<IdentIndex>,
    imports: Vec<SpecIndex>, //ImportSpec

    label_scope: Option<ScopeIndex>,
    target_stack: Vec<Vec<IdentIndex>>,
}

impl<'a> Parser<'a> {
    fn new(file: &'a mut position::File, src: &'a str, trace: bool) -> Parser<'a> {
        let err = Rc::new(RefCell::new(errors::ErrorList::new()));
        let s = scanner::Scanner::new(file, src, err.clone());
        Parser{
            objects: Objects::new(),
            scanner: s,
            errors: err,
            trace: trace,
            indent: 0,
            pos: 0,
            token: Token::ILLEGAL("".to_string()),
            sync_pos: 0,
            sync_count: 0,
            expr_level: 0,
            in_rhs: false,
            pkg_scope: None,
            top_scope: None,
            unresolved: vec![],
            imports: vec![],
            label_scope:None,
            target_stack: vec![],
        }
    }

    // ----------------------------------------------------------------------------
    // Scoping support

    fn open_scope(&mut self) {
        self.top_scope = 
            Some(Scope::arena_new(self.top_scope.take(), scopes_mut!(self)));
    }

    fn close_scope(&mut self) {
        self.top_scope = scope!(self, self.top_scope.take().unwrap()).outer;
    }

    fn open_label_scope(&mut self) {
        self.label_scope = 
            Some(Scope::arena_new(self.label_scope.take(), scopes_mut!(self)));
        self.target_stack.push(vec![]);
    }

    fn close_label_scope(&mut self) {
        let scope = scope!(self, *self.label_scope.as_ref().unwrap());
        match self.target_stack.pop() {
            Some(v) => {
                for i in v {
                    let ident = ident!(self, i);
                    if scope.look_up(&ident.name, entities_mut!(self)).is_none() {
                        let s = format!("label {} undefined", ident.name);
                        self.error(self.pos, s);
                    }
                }
            }
            _ => panic!("invalid target stack.")
        }
        self.label_scope = scope!(self, self.label_scope.take().unwrap()).outer;
    }

    fn declare(&mut self, decl: DeclObj, data: EntityData, kind: EntityKind,
        scope_ind: &ScopeIndex, idents: Vec<IdentIndex>) {
        for id in idents.iter() {
            let mut_ident = ident_mut!(self, *id);
            let entity = Entity::arena_new(kind.clone(), mut_ident.name.clone(),
                decl.clone(), data.clone(), entities_mut!(self));
            mut_ident.entity = IdentEntity::Entity(entity);
            let ident = ident!(self, *id);
            if ident.name != "_" {
                let scope = scope_mut!(self, *scope_ind);
                match scope.insert(ident.name.clone(), entity) {
                    Some(prev_decl) => {
                        let p =  entity!(self, prev_decl).pos(&self.objects);
                        let mut buf = String::new();
                        fmt::write(&mut buf, format_args!(
                            "{} redeclared in this block\n\tprevious declaration at {}",
                            ident.name, 
                            self.file().position(p))).unwrap();
                        self.error(ident.pos, buf);
                    },
                    _ => {},
                }
            }
        }
    }

    fn short_var_decl(&mut self, assign_stmt: StmtIndex, list: Vec<Expr>) {
        // Go spec: A short variable declaration may redeclare variables
        // provided they were originally declared in the same block with
        // the same type, and at least one of the non-blank variables is new.
	    let mut n = 0; // number of new variables
        for expr in &list {
            match expr {
                Expr::Ident(id) => {
                    let ident = ident_mut!(self, *id.as_ref());
                    let entity = Entity::arena_new(EntityKind::Var, 
                        ident.name.clone(), DeclObj::Stmt(assign_stmt),
                        EntityData::NoData, entities_mut!(self));
                    ident.entity = IdentEntity::Entity(entity);
                    if ident.name != "_" {
                        let top_scope = scope_mut!(self, self.top_scope.unwrap());
                        match top_scope.insert(ident.name.clone(), entity) {
                            Some(e) => { ident.entity = IdentEntity::Entity(e); },
                            None => { n += 1; },
                        }
                    }
                },
                _ => {
                    self.error_expected(expr.pos(&self.objects), 
                        "identifier on left side of :=");
                },
            }
        }
        if n == 0 {
            self.error(list[0].pos(&self.objects), 
                "no new variables on left side of :=".to_string())
        }
    }

    // If x is an identifier, tryResolve attempts to resolve x by looking up
    // the object it denotes. If no object is found and collectUnresolved is
    // set, x is marked as unresolved and collected in the list of unresolved
    // identifiers.
    fn try_resolve(&mut self, x: &Expr, collect_unresolved: bool) {
        if let Expr::Ident(i) = x {
            let ident = ident_mut!(self, *i.as_ref());
            assert!(ident.entity.is_none(), 
                "identifier already declared or resolved");
            // all local scopes are known, so any unresolved identifier
            // must be found either in the file scope, package scope
            // (perhaps in another file), or universe scope --- collect
            // them so that they can be resolved later
            if collect_unresolved {
                ident.entity = IdentEntity::Sentinel;
                self.unresolved.push(*i.as_ref());
            }
        }
    }

    fn resolve(&mut self, x: &Expr) {
        self.try_resolve(x, true)
    }

    // ----------------------------------------------------------------------------
    // Parsing support

    fn file_mut(&mut self) -> &mut position::File {
        self.scanner.file_mut()
    }

    fn file(&self) -> &position::File {
        self.scanner.file()
    }

    fn print_trace(&self, msg: &str) {
        let f = self.file();
        let p = f.position(self.pos);
        let mut buf = String::new();
        fmt::write(&mut buf, format_args!("{:5o}:{:3o}:", p.line, p.column)).unwrap();
        for _ in 0..self.indent {
            buf.push_str("..");
        }
        print!("{}{}\n", buf, msg);
    }

    fn trace_begin(&mut self, msg: &str) {
        if self.trace {
            let mut trace_str = msg.to_string();
            trace_str.push('(');
            self.print_trace(&trace_str);
            self.indent += 1;
        }
    }

    fn trace_end(&mut self) {
        if self.trace {
            self.indent -= 1;
            self.print_trace(")");
        }
    }

    fn next(&mut self) {
        // Print previous token
        if self.pos > 0 {
            self.print_trace(&format!("{}", self.token));
        }
        // Get next token and skip comments
        let mut token: Token;
        loop {
            token = self.scanner.scan();
            match token {
                Token::COMMENT(_) => { // Skip comment
                    self.print_trace(&format!("{}", self.token));
                },
                _ => { break; },
            }
        }
        self.token = token;
        self.pos = self.scanner.pos();
    }

    fn error(&self, pos: position::Pos, msg: String) {
        let p = self.file().position(pos);
        self.errors.borrow_mut().add(p, msg)
    }

    fn error_expected(&self, pos: position::Pos, msg: &str) {
        let mut mstr = "expected ".to_string();
        mstr.push_str(msg);
        if pos == self.pos {
            match self.token {
                Token::SEMICOLON(real) => if !real {
                    mstr.push_str(", found newline");
                },
                _ => {
                    mstr.push_str(", found ");
                    mstr.push_str(self.token.token_text());
                }
            }
        }
        self.error(pos, mstr);
    }

    fn expect(&mut self, token: &Token) -> position::Pos {
        let pos = self.pos;
        if self.token != *token {
            self.error_expected(pos, &format!("'{}'", token));
        }
        self.next();
        pos
    }

    // https://github.com/golang/go/issues/3008
    // Same as expect but with better error message for certain cases
    fn expect_closing(&mut self, token: &Token, context: &str) -> position::Pos {
        if let Token::SEMICOLON(real) = token {
            if !real {
                let msg = format!("missing ',' before newline in {}", context);
                self.error(self.pos, msg);
                self.next();
            }
        }
        self.expect(token)
    }

    fn expect_semi(&mut self) {
        // semicolon is optional before a closing ')' or '}'
        match self.token {
            Token::RPAREN | Token::RBRACE => {},
            Token::SEMICOLON(_) => { self.next(); },
            _ => {
                if let Token::COMMA = self.token {
                    // permit a ',' instead of a ';' but complain
                    self.error_expected(self.pos, "';'");
                    self.next();
                }
                self.error_expected(self.pos, "';'");
                self.sync_stmt();
            }
        }
    }

    fn at_comma(&self, context: &str, follow: &Token) -> bool {
        if let Token::COMMA = self.token {
            true
        } else if self.token == *follow {
            let mut msg =  "missing ','".to_string();
            if let Token::SEMICOLON(real) = self.token {
                if !real {msg.push_str(" before newline");}
            }
            msg = format!("{} in {}", msg, context);
            self.error(self.pos, msg);
            true
        } else {
            false
        }
    }

    // syncStmt advances to the next statement.
    // Used for synchronization after an error.
    fn sync_stmt(&mut self) {
        loop {
            match self.token {
                Token::BREAK | Token::CONST | Token::CONTINUE | Token::DEFER |
			    Token::FALLTHROUGH | Token::FOR | Token::GO | Token::GOTO | 
			    Token::IF | Token::RETURN | Token::SELECT | Token::SWITCH |
			    Token::TYPE | Token::VAR => {
                    // Return only if parser made some progress since last
                    // sync or if it has not reached 10 sync calls without
                    // progress. Otherwise consume at least one token to
                    // avoid an endless parser loop (it is possible that
                    // both parseOperand and parseStmt call syncStmt and
                    // correctly do not advance, thus the need for the
                    // invocation limit p.syncCnt).
                    if self.pos == self.sync_pos && self.sync_count < 10 {
                        self.sync_count += 1;
                        return;
                    }
                    if self.pos > self.sync_pos {
                        self.sync_pos = self.pos;
                        self.sync_count = 0;
                        return;
                    }
                },
                // Reaching here indicates a parser bug, likely an
                // incorrect token list in this function, but it only
                // leads to skipping of possibly correct code if a
                // previous error is present, and thus is preferred
                // over a non-terminating parse.
                Token::EOF => { return; },
                _ => {},
            }
            self.next();
        }
    }

    // syncDecl advances to the next declaration.
    // Used for synchronization after an error.
    fn sync_decl(&mut self) {
        loop {
            match self.token {
                Token::CONST | Token::TYPE | Token::VAR => {
                    // same as sync_stmt
                    if self.pos == self.sync_pos && self.sync_count < 10 {
                        self.sync_count += 1;
                        return;
                    }
                    if self.pos > self.sync_pos {
                        self.sync_pos = self.pos;
                        self.sync_count = 0;
                        return;
                    }
                }
                Token::EOF => { return; },
                _ => {},
            }
            self.next();
        }
    }

    // safe_pos returns a valid file position for a given position: If pos
    // is valid to begin with, safe_pos returns pos. If pos is out-of-range,
    // safe_pos returns the EOF position.
    //
    // This is hack to work around "artificial" end positions in the AST which
    // are computed by adding 1 to (presumably valid) token positions. If the
    // token positions are invalid due to parse errors, the resulting end position
    // may be past the file's EOF position, which would lead to panics if used
    // later on.
    fn safe_pos(&self, pos: position::Pos) -> position::Pos {
        let max = self.file().base() + self.file().size(); 
        if pos > max { max } else { pos }
    }

    // ----------------------------------------------------------------------------
    // Identifiers

    fn parse_ident(&mut self) -> IdentIndex {
        let pos = self.pos;
        let mut name = "_".to_string();
        if let Token::IDENT(lit) = self.token.clone() {
            name = lit;
            self.next();
        } else {
            self.expect(&Token::IDENT("".to_string()));
        }
        self.objects.idents.insert(Ident{ pos: pos, name: name,
            entity: IdentEntity::NoEntity})
    }

    fn parse_ident_list(&mut self) -> Vec<IdentIndex> {
        self.trace_begin("IdentList");
        
        let mut list = vec![self.parse_ident()];
        while self.token == Token::COMMA {
            self.next();
            list.push(self.parse_ident());
        }
       
        self.trace_end();
        list
    }
    
    fn parse(&mut self) {
        self.trace_begin("begin");
        print!("222xxxxxxx \n");
        self.trace_end();
    }
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn test_parser () {
        let fs = position::SharedFileSet::new();
        let mut fsm = fs.borrow_mut();
        let f = fsm.add_file(fs.weak(), "testfile1.gs", 0, 1000);

        let mut p = Parser::new(f, "", true);
        p.parse();
    }
} 