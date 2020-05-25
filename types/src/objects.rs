#![macro_use]
#![allow(unused_macros)]
#![allow(dead_code)]
use super::obj::LangObj;
use super::package::Package;
use super::scope::Scope;
use super::typ::Type;
use super::universe::Universe;
use goscript_parser::position;
use std::borrow::Cow;

use slotmap::{new_key_type, DenseSlotMap};

const DEFAULT_CAPACITY: usize = 16;

macro_rules! new_objects {
    () => {
        DenseSlotMap::with_capacity_and_key(DEFAULT_CAPACITY)
    };
}

new_key_type! { pub struct ObjKey; }
new_key_type! { pub struct TypeKey; }
new_key_type! { pub struct PackageKey; }
new_key_type! { pub struct ScopeKey; }

pub type LangObjs = DenseSlotMap<ObjKey, LangObj>;
pub type Types = DenseSlotMap<TypeKey, Type>;
pub type Packages = DenseSlotMap<PackageKey, Package>;
pub type Scopes = DenseSlotMap<ScopeKey, Scope>;

/// The container of all "managed" objects
/// also works as a "global" variable holder
pub struct TCObjects {
    pub lobjs: LangObjs,
    pub types: Types,
    pub pkgs: Packages,
    pub scopes: Scopes,
    pub universe: Universe,
    // "global" variable
    pub debug: bool,
    // "global" variable
    pub fmt_qualifier: Box<dyn Fn(&Package) -> Cow<str>>,
}

fn default_fmt_qualifier(p: &Package) -> Cow<str> {
    p.path().into()
}

impl TCObjects {
    pub fn new() -> TCObjects {
        let mut scopes = new_objects!();
        let skey = scopes.insert(Scope::new(None, 0, 0, "universe".to_owned()));
        let mut pkgs = new_objects!();
        let pkg_skey = scopes.insert(Scope::new(Some(skey), 0, 0, "package unsafe".to_owned()));
        let pkey = pkgs.insert(Package::new(
            "unsafe".to_owned(),
            "unsafe".to_owned(),
            pkg_skey,
        ));
        let fmtq = Box::new(default_fmt_qualifier);
        TCObjects {
            lobjs: new_objects!(),
            types: new_objects!(),
            pkgs: pkgs,
            scopes: scopes,
            universe: Universe::new(skey, pkey),
            debug: true,
            fmt_qualifier: fmtq,
        }
    }

    pub fn new_scope(
        &mut self,
        parent: Option<ScopeKey>,
        pos: position::Pos,
        end: position::Pos,
        comment: String,
    ) -> ScopeKey {
        let scope = Scope::new(parent, pos, end, comment);
        let skey = self.scopes.insert(scope);
        if let Some(skey) = parent {
            // don't add children to Universe scope
            if skey != *self.universe.scope() {
                self.scopes[skey].add_child(skey);
            }
        }
        skey
    }

    pub fn new_package(&mut self, path: String, name: String) -> PackageKey {
        let skey = self.new_scope(
            Some(*self.universe.scope()),
            0,
            0,
            format!("package {}", path),
        );
        let pkg = Package::new(path, name, skey);
        self.pkgs.insert(pkg)
    }
}