use std::mem;

use crate::re::instr::{
    decode_instr, epsilon_closure, CodeLoc, EpsilonClosureState, Instr,
};

/// Represents a [Pike's VM](https://swtch.com/~rsc/regexp/regexp2.html) that
/// executes VM code produced by the [compiler][`crate::re::compiler::Compiler`].
pub(crate) struct PikeVM {
    /// The list of currently active threads. Each item in this list is a
    /// position within the VM code, pointing to some VM instruction. Each item
    /// in the list is unique, the VM guarantees that there aren't two active
    /// threads at the same VM instruction.
    threads: Vec<usize>,
    /// The list of threads that will become the active threads when the next
    /// byte is read from the input.
    next_threads: Vec<usize>,
    cache: EpsilonClosureState,
}

impl PikeVM {
    /// Creates a new [`PikeVM`].
    pub fn new() -> Self {
        Self {
            threads: Vec::new(),
            next_threads: Vec::new(),
            cache: EpsilonClosureState::new(),
        }
    }

    /// Executes VM code starting at the `start` location and returns the
    /// number of bytes from `fwd_input` that matched. The number of bytes
    /// returned can be zero if the VM matches a zero-length string. Returns
    /// `None` if the data read from input don't match the regexp.
    ///
    /// `bck_input` is an iterator that returns the bytes that were before
    /// the stating point of `fwd_input`, in reverse order. For instance,
    /// suppose we have the string `a b c e f g h i`, and `fwd_input` starts
    /// at the `f` character and returns `f`, `g`, `h` and `i` in that order.
    /// In such case `bck_input` will return `e`, `c`, `b` and `a`.
    ///
    /// ```text
    ///       a  b  c  e  f   g   h   i
    ///                   |  
    ///      <- bck_input | fwd_input ->
    /// ```
    ///
    /// The purpose of `bck_input` is allowing the function to access the bytes
    /// that appear right before the start of `fwd_input` for matching some
    /// look-around assertions that need information about the surrounding
    /// bytes.
    pub(crate) fn try_match<'a, C, F, B>(
        &mut self,
        code: &[u8],
        start: C,
        mut fwd_input: F,
        mut bck_input: B,
    ) -> Option<usize>
    where
        C: CodeLoc,
        F: Iterator<Item = &'a u8>,
        B: Iterator<Item = &'a u8>,
    {
        let step = 1;
        let mut matched_bytes = None;
        let mut current_pos = 0;
        let mut byte = fwd_input.next();

        epsilon_closure(
            code,
            start,
            byte,
            bck_input.next(),
            &mut self.cache,
            &mut self.threads,
        );

        while !self.threads.is_empty() {
            let next_byte = fwd_input.next();

            for ip in self.threads.iter() {
                let (instr, size) = decode_instr(&code[*ip..]);

                let is_match = match instr {
                    Instr::AnyByte => byte.is_some(),
                    Instr::Byte(expected) => {
                        matches!(byte, Some(byte) if *byte == expected)
                    }
                    Instr::MaskedByte(expected, mask) => {
                        matches!(byte, Some(byte) if *byte & mask == expected)
                    }
                    Instr::ClassBitmap(class) => {
                        matches!(byte, Some(byte) if class.contains(*byte))
                    }
                    Instr::ClassRanges(class) => {
                        matches!(byte, Some(byte) if class.contains(*byte))
                    }
                    Instr::Match => {
                        matched_bytes = Some(current_pos);
                        // if non-greedy break
                        break;
                    }
                    Instr::Eoi => {
                        // TODO: is this correct?
                        break;
                    }
                    _ => unreachable!(),
                };

                if is_match {
                    epsilon_closure(
                        code,
                        C::from(*ip + size),
                        next_byte,
                        byte,
                        &mut self.cache,
                        &mut self.next_threads,
                    );
                }
            }

            byte = next_byte;
            current_pos += step;
            mem::swap(&mut self.threads, &mut self.next_threads);
            self.next_threads.clear();
        }

        matched_bytes
    }
}