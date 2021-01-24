use super::atom::*;
use super::consts::*;
use super::engine::*;
use super::functor::*;
use super::module::*;
use super::predicate::*;
use super::term::*;

use std::convert::TryInto;
use std::sync::atomic::{AtomicBool, Ordering};
use swipl_sys::*;

pub struct Context<'a, T: ContextType> {
    parent: Option<&'a dyn ContextParent>,
    context: T,
    engine: PL_engine_t,
    activated: AtomicBool,
}

impl<'a, T: ContextType> Context<'a, T> {
    fn assert_activated(&self) {
        if !self.activated.load(Ordering::Relaxed) {
            panic!("cannot acquire term refs from inactive context");
        }
    }
    pub fn engine_ptr(&self) -> PL_engine_t {
        self.engine
    }

    /// Creates a new 'unknown' context with the same lifetime as this context.
    ///
    /// This is primarily useful for storing in terms, where we wish to keep track of a context without term itself being generic. The unknown context shares a lifetime with this context, but will do nothing on destruction (as 'self' already does all necessary cleanup).
    pub fn as_unknown(&self) -> Context<Unknown> {
        Context {
            parent: None,
            context: Unknown { _x: false },
            engine: self.engine,
            activated: AtomicBool::new(true),
        }
    }

    pub fn new_term_ref(&self) -> Term {
        self.assert_activated();
        unsafe {
            let term = PL_new_term_ref();
            Term::new(term, self)
        }
    }

    pub unsafe fn wrap_term_ref(&self, term: term_t) -> Term {
        self.assert_activated();
        Term::new(term, self)
    }

    pub fn open_frame(&self) -> Context<Frame> {
        self.assert_activated();
        let fid = unsafe { PL_open_foreign_frame() };

        let frame = Frame {
            fid,
            state: FrameState::Active,
        };

        self.activated.store(false, Ordering::Relaxed);
        Context {
            parent: Some(self),
            context: frame,
            engine: self.engine,
            activated: AtomicBool::new(true),
        }
    }
}

trait ContextParent {
    fn reactivate(&self);
}

impl<'a, T: ContextType> ContextParent for Context<'a, T> {
    fn reactivate(&self) {
        if self
            .activated
            .compare_and_swap(false, true, Ordering::Acquire)
        {
            panic!("context already active");
        }
    }
}

impl<'a, T: ContextType> TermOrigin for Context<'a, T> {
    fn is_engine_active(&self) -> bool {
        is_engine_active(self.engine)
    }

    fn origin_engine_ptr(&self) -> PL_engine_t {
        self.engine
    }

    fn context(&self) -> Context<Unknown> {
        self.as_unknown()
    }
}

impl<'a, T: ContextType> Drop for Context<'a, T> {
    fn drop(&mut self) {
        if let Some(parent) = self.parent {
            parent.reactivate();
        }
    }
}

pub trait ContextType {}

pub struct ActivatedEngine<'a> {
    _activation: EngineActivation<'a>,
}

impl<'a> Into<Context<'a, ActivatedEngine<'a>>> for EngineActivation<'a> {
    fn into(self) -> Context<'a, ActivatedEngine<'a>> {
        let engine = self.engine_ptr();
        let context = ActivatedEngine { _activation: self };

        Context {
            parent: None,
            context,
            engine,
            activated: AtomicBool::new(true),
        }
    }
}

impl<'a> ContextType for ActivatedEngine<'a> {}

pub struct UnmanagedContext {
    // only here to prevent automatic construction
    _x: bool,
}
impl ContextType for UnmanagedContext {}

// This is unsafe to call if we are not in a swipl environment, or if some other context is active. Furthermore, the lifetime will most definitely be wrong. This should be used by code that doesn't promiscuously spread this context. all further accesses should be through borrows.
pub unsafe fn unmanaged_engine_context() -> Context<'static, UnmanagedContext> {
    let current = current_engine_ptr();

    if current.is_null() {
        panic!("tried to create an unmanaged engine context, but no engine is active");
    }

    Context {
        parent: None,
        context: UnmanagedContext { _x: false },
        engine: current,
        activated: AtomicBool::new(true),
    }
}

pub struct Unknown {
    // only here to prevent automatic construction
    _x: bool,
}
impl ContextType for Unknown {}

enum FrameState {
    Active,
    Discarded,
}

pub struct Frame {
    fid: PL_fid_t,
    state: FrameState,
}

impl ContextType for Frame {}

impl Drop for Frame {
    fn drop(&mut self) {
        match &self.state {
            FrameState::Active =>
            // unsafe justification: all instantiations of Frame happen in
            // this module.  This module only instantiates the frame as
            // part of the context mechanism. No 'free' Frames are ever
            // returned.  This mechanism ensures that the frame is only
            // closed if there's no inner frame still remaining. It'll
            // also ensure that the engine of the frame is active while
            // dropping.
            unsafe { PL_close_foreign_frame(self.fid) }
            _ => {}
        }
    }
}

impl<'a> Context<'a, Frame> {
    pub fn close_frame(self) {
        // would happen automatically but might as well be explicit
        std::mem::drop(self)
    }

    pub fn discard_frame(mut self) {
        self.context.state = FrameState::Discarded;
        // unsafe justification: reasons for safety are the same as in a normal drop. Also, sicne we just set framestate to discarded, the drop won't try to subsequently close this same frame.
        unsafe { PL_discard_foreign_frame(self.context.fid) };
    }

    pub fn rewind_frame(&self) {
        self.assert_activated();
        // unsafe justification: We just checked that this frame right here is currently the active context. Therefore it can be rewinded.
        unsafe { PL_rewind_foreign_frame(self.context.fid) };
    }
}

pub unsafe trait ActiveEnginePromise: Sized {
    fn new_atom(&self, name: &str) -> Atom {
        unsafe { Atom::new(name) }
    }

    fn new_functor<A: IntoAtom>(&self, name: A, arity: u16) -> Functor {
        if arity as usize > MAX_ARITY {
            panic!("functor arity is >1024: {}", arity);
        }
        let atom = name.into_atom(self);

        let functor = unsafe { PL_new_functor(atom.atom_ptr(), arity.try_into().unwrap()) };

        unsafe { Functor::wrap(functor) }
    }

    fn new_module<A: IntoAtom>(&self, name: A) -> Module {
        unsafe { Module::new(name) }
    }

    fn new_predicate(&self, functor: &Functor, module: &Module) -> Predicate {
        unsafe { Predicate::new(functor, module) }
    }
}

unsafe impl<'a> ActiveEnginePromise for EngineActivation<'a> {}
unsafe impl<'a, C: ContextType> ActiveEnginePromise for Context<'a, C> {}
unsafe impl<'a> ActiveEnginePromise for &'a dyn TermOrigin {}

pub struct UnsafeActiveEnginePromise {
    _x: bool,
}

impl UnsafeActiveEnginePromise {
    pub unsafe fn new() -> Self {
        Self { _x: false }
    }
}

unsafe impl ActiveEnginePromise for UnsafeActiveEnginePromise {}

pub struct Query {
    qid: qid_t,
    closed: bool,
}

impl ContextType for Query {}

pub unsafe trait QueryableContextType: ContextType {}
unsafe impl<'a> QueryableContextType for ActivatedEngine<'a> {}
unsafe impl QueryableContextType for Frame {}

impl<'a, T: QueryableContextType> Context<'a, T> {
    pub fn open_query(
        &self,
        context: Option<&Module>,
        predicate: &Predicate,
        args: &[&Term],
    ) -> Context<Query> {
        self.assert_activated();
        let context = context
            .map(|c| c.module_ptr())
            .unwrap_or(std::ptr::null_mut());
        let flags = PL_Q_NORMAL | PL_Q_EXT_STATUS;
        let terms = unsafe { PL_new_term_refs(args.len().try_into().unwrap()) };
        for i in 0..args.len() {
            let term = unsafe { self.wrap_term_ref(terms + i) };
            assert!(term.unify(args[i]));
        }

        let qid = unsafe {
            PL_open_query(
                context,
                flags.try_into().unwrap(),
                predicate.predicate_ptr(),
                terms,
            )
        };

        let query = Query { qid, closed: false };

        self.activated.store(false, Ordering::Relaxed);
        Context {
            parent: Some(self),
            context: query,
            engine: self.engine,
            activated: AtomicBool::new(true),
        }
    }

    pub fn term_from_string(&self, s: &str) -> Option<Term> {
        let term = self.new_term_ref();

        // TODO: must cache this
        let functor_read_term_from_atom = self.new_functor("read_term_from_atom", 3);
        let module = self.new_module("user");
        let predicate = self.new_predicate(&functor_read_term_from_atom, &module);

        // TODO we could do with less terms since open_query is going to recreate them
        let arg1 = self.new_term_ref();
        let arg3 = self.new_term_ref();

        assert!(arg1.unify(s));
        assert!(arg3.unify(Nil));

        let query = self.open_query(None, &predicate, &[&arg1, &term, &arg3]);
        let result = match query.next_solution() {
            QueryResult::SuccessLast => Some(term),
            _ => None,
        };
        query.cut();

        result
    }

    pub fn open_call(&self, t: &Term) -> Context<Query> {
        // TODO: must cache this
        let functor_call = self.new_functor("call", 1);
        let module = self.new_module("user");
        let predicate = self.new_predicate(&functor_call, &module);

        self.open_query(None, &predicate, &[&t])
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum QueryResult {
    Success,
    SuccessLast,
    Failure,
    Exception,
}

impl<'a> Context<'a, Query> {
    pub fn next_solution(&self) -> QueryResult {
        let result = unsafe { PL_next_solution(self.context.qid) };
        // TODO handle exceptions properly
        match result {
            -1 => QueryResult::Exception,
            0 => QueryResult::Failure,
            1 => QueryResult::Success,
            2 => QueryResult::SuccessLast,
            _ => panic!("unknown query result type {}", result),
        }
    }

    pub fn cut(mut self) {
        // TODO handle exceptions
        unsafe { PL_cut_query(self.context.qid) };
        self.context.closed = true;
    }

    pub fn discard(mut self) {
        // TODO handle exceptions

        unsafe { PL_close_query(self.context.qid) };
        self.context.closed = true;
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // honestly, since closing a query may result in exceptions,
        // this is too late. We'll just assume the user intended to
        // discard, to encourage proper closing.
        if !self.closed {
            unsafe { PL_close_query(self.qid) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn get_term_ref_on_fresh_engine() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let _term = context.new_term_ref();
    }

    #[test]
    fn get_term_ref_on_frame() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context1: Context<_> = activation.into();
        let _term1 = context1.new_term_ref();

        let context2 = context1.open_frame();
        let _term2 = context2.new_term_ref();
        std::mem::drop(context2);
        let _term3 = context1.new_term_ref();
    }

    #[test]
    #[should_panic]
    fn get_term_ref_from_inactive_context_panics() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context1: Context<_> = activation.into();
        let _context2 = context1.open_frame();

        let _term = context1.new_term_ref();
    }

    #[test]
    fn query_det() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let functor_is = context.new_functor("is", 2);
        let functor_plus = context.new_functor("+", 2);
        let module = context.new_module("user");
        let predicate = context.new_predicate(&functor_is, &module);

        let term1 = context.new_term_ref();
        let term2 = context.new_term_ref();

        assert!(term2.unify(&functor_plus));
        assert!(term2.unify_arg(1, 40_u64));
        assert!(term2.unify_arg(2, 2_u64));

        let query = context.open_query(None, &predicate, &[&term1, &term2]);
        let next = query.next_solution();

        assert_eq!(QueryResult::SuccessLast, next);
        assert_eq!(42_u64, term1.get().unwrap());

        let next = query.next_solution();
        assert_eq!(QueryResult::Failure, next);
    }

    #[test]
    fn query_auto_discard() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let functor_is = context.new_functor("is", 2);
        let functor_plus = context.new_functor("+", 2);
        let module = context.new_module("user");
        let predicate = context.new_predicate(&functor_is, &module);

        let term1 = context.new_term_ref();
        let term2 = context.new_term_ref();

        assert!(term2.unify(&functor_plus));
        assert!(term2.unify_arg(1, 40_u64));
        assert!(term2.unify_arg(2, 2_u64));

        {
            let query = context.open_query(None, &predicate, &[&term1, &term2]);
            let next = query.next_solution();

            assert_eq!(QueryResult::SuccessLast, next);
            assert_eq!(42_u64, term1.get().unwrap());
        }

        // after leaving the block, we have discarded
        assert!(term1.get::<u64>().is_none());
    }

    #[test]
    fn query_manual_discard() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let functor_is = context.new_functor("is", 2);
        let functor_plus = context.new_functor("+", 2);
        let module = context.new_module("user");
        let predicate = context.new_predicate(&functor_is, &module);

        let term1 = context.new_term_ref();
        let term2 = context.new_term_ref();

        assert!(term2.unify(&functor_plus));
        assert!(term2.unify_arg(1, 40_u64));
        assert!(term2.unify_arg(2, 2_u64));

        {
            let query = context.open_query(None, &predicate, &[&term1, &term2]);
            let next = query.next_solution();

            assert_eq!(QueryResult::SuccessLast, next);
            assert_eq!(42_u64, term1.get().unwrap());
            query.discard();
        }

        // after leaving the block, we have discarded
        assert!(term1.get::<u64>().is_none());
    }

    #[test]
    fn query_cut() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let functor_is = context.new_functor("is", 2);
        let functor_plus = context.new_functor("+", 2);
        let module = context.new_module("user");
        let predicate = context.new_predicate(&functor_is, &module);

        let term1 = context.new_term_ref();
        let term2 = context.new_term_ref();

        assert!(term2.unify(&functor_plus));
        assert!(term2.unify_arg(1, 40_u64));
        assert!(term2.unify_arg(2, 2_u64));

        {
            let query = context.open_query(None, &predicate, &[&term1, &term2]);
            let next = query.next_solution();

            assert_eq!(QueryResult::SuccessLast, next);
            assert_eq!(42_u64, term1.get().unwrap());
            query.cut();
        }

        // a cut query leaves data intact
        assert_eq!(42_u64, term1.get().unwrap());
    }

    #[test]
    fn term_from_string_works() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("foo(bar(baz,quux))").unwrap();
        let functor_foo = context.new_functor("foo", 1);
        let functor_bar = context.new_functor("bar", 2);

        assert_eq!(functor_foo, term.get().unwrap());
        assert_eq!(functor_bar, term.get_arg(1).unwrap());
    }

    #[test]
    fn open_call_nondet() {
        initialize_swipl_noengine();
        let engine = Engine::new();
        let activation = engine.activate();
        let context: Context<_> = activation.into();

        let term = context.term_from_string("member(X, [a,b,c])").unwrap();
        let term_x = context.new_term_ref();
        assert!(term.unify_arg(1, &term_x));

        let query = context.open_call(&term);
        assert_eq!(QueryResult::Success, query.next_solution());
        term_x.get_atomable(|a| assert_eq!("a", a.unwrap().name()));

        assert_eq!(QueryResult::Success, query.next_solution());
        term_x.get_atomable(|a| assert_eq!("b", a.unwrap().name()));

        assert_eq!(QueryResult::SuccessLast, query.next_solution());
        term_x.get_atomable(|a| assert_eq!("c", a.unwrap().name()));

        assert_eq!(QueryResult::Failure, query.next_solution());
    }
}
