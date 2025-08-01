/*! A Python extension module for YARA-X.

This crate implements a Python module for using YARA-X from Python. It allows
compiling YARA rules and scanning data and files with those rules. Supports
Python 3.8+.

# Usage

```python
import yara_x
rules = yara_x.compile('rule test {strings: $a = "dummy" condition: $a}')
matches = rules.scan(b'some dummy data')
```
 */

#![deny(missing_docs)]

use std::marker::PhantomPinned;
use std::mem;
use std::ops::Deref;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;
use strum_macros::{Display, EnumString};

use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use protobuf::MessageDyn;
use pyo3::exceptions::{PyException, PyIOError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{
    PyBool, PyBytes, PyDict, PyFloat, PyInt, PyString, PyStringMethods,
    PyTuple, PyTzInfo,
};
use pyo3::{create_exception, IntoPyObjectExt};
use pyo3_file::PyFileLikeObject;

use ::yara_x as yrx;
use ::yara_x::mods::*;

#[derive(Debug, Clone, Display, EnumString, PartialEq)]
#[strum(ascii_case_insensitive)]
enum SupportedModules {
    Lnk,
    Macho,
    Elf,
    Pe,
    Dotnet,
}

/// Formats YARA rules.
#[pyclass(unsendable)]
struct Formatter {
    inner: yara_x_fmt::Formatter,
}

#[pymethods]
impl Formatter {
    /// Creates a new [`Formatter`].
    ///
    /// `align_metadata` allows for aligning the equals signs in metadata definitions.
    /// `align_patterns` allows for aligning the equals signs in pattern definitions.
    /// `indent_section_headers` allows for indenting section headers.
    /// `indent_section_contents` allows for indenting section contents.
    /// `indent_spaces` is the number of spaces to use for indentation.
    /// `newline_before_curly_brace` controls whether a newline is inserted before a curly brace.
    /// `empty_line_before_section_header` controls whether an empty line is inserted before a section header.
    /// `empty_line_after_section_header` controls whether an empty line is inserted after a section header.
    #[new]
    #[pyo3(signature = (
        align_metadata = true,
        align_patterns = true,
        indent_section_headers = true,
        indent_section_contents = true,
        indent_spaces = 2,
        newline_before_curly_brace = false,
        empty_line_before_section_header = true,
        empty_line_after_section_header = false
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        align_metadata: bool,
        align_patterns: bool,
        indent_section_headers: bool,
        indent_section_contents: bool,
        indent_spaces: u8,
        newline_before_curly_brace: bool,
        empty_line_before_section_header: bool,
        empty_line_after_section_header: bool,
    ) -> Self {
        Self {
            inner: yara_x_fmt::Formatter::new()
                .align_metadata(align_metadata)
                .align_patterns(align_patterns)
                .indent_section_headers(indent_section_headers)
                .indent_section_contents(indent_section_contents)
                .indent_spaces(indent_spaces)
                .newline_before_curly_brace(newline_before_curly_brace)
                .empty_line_before_section_header(
                    empty_line_before_section_header,
                )
                .empty_line_after_section_header(
                    empty_line_after_section_header,
                ),
        }
    }

    /// Format a YARA rule
    fn format(&self, input: PyObject, output: PyObject) -> PyResult<()> {
        let in_buf = PyFileLikeObject::with_requirements(
            input, true, false, false, false,
        )?;

        let mut out_buf = PyFileLikeObject::with_requirements(
            output, false, true, false, false,
        )?;

        self.inner
            .format(in_buf, &mut out_buf)
            .map_err(|err| PyValueError::new_err(err.to_string()))?;

        Ok(())
    }
}

#[pyclass]
struct Module {
    _module: SupportedModules,
}

#[pymethods]
impl Module {
    /// Creates a new [`Module`].
    ///
    /// Type of module must be one of [`SupportedModules`]
    #[new]
    fn new(name: &str) -> PyResult<Self> {
        Ok(Self {
            _module: SupportedModules::from_str(name).map_err(|_| {
                PyValueError::new_err(format!("{name} not a supported module"))
            })?,
        })
    }

    /// Invoke the module with provided data.
    ///
    /// Returns None if the module didn't produce any output for the given data.
    fn invoke<'py>(
        &'py self,
        py: Python<'py>,
        data: &[u8],
    ) -> PyResult<Bound<'py, PyAny>> {
        let module_output = match self._module {
            SupportedModules::Macho => invoke_dyn::<Macho>(data),
            SupportedModules::Lnk => invoke_dyn::<Lnk>(data),
            SupportedModules::Elf => invoke_dyn::<ELF>(data),
            SupportedModules::Pe => invoke_dyn::<PE>(data),
            SupportedModules::Dotnet => invoke_dyn::<Dotnet>(data),
        };

        let module_output = match module_output {
            Some(output) => output,
            None => return Ok(py.None().into_bound(py)),
        };

        proto_to_json(py, module_output.as_ref())
    }
}

/// Compiles a YARA source code producing a set of compiled [`Rules`].
///
/// This function allows compiling simple rules that don't depend on external
/// variables. For more complex use cases you will need to use a [`Compiler`].
#[pyfunction]
fn compile(src: &str) -> PyResult<Rules> {
    let rules = yrx::compile(src)
        .map_err(|err| CompileError::new_err(err.to_string()))?;

    Ok(Rules::new(rules))
}

/// Compiles YARA source code producing a set of compiled [`Rules`].
#[pyclass(unsendable)]
struct Compiler {
    inner: yrx::Compiler<'static>,
    relaxed_re_syntax: bool,
    error_on_slow_pattern: bool,
    includes_enabled: bool,
}

impl Compiler {
    fn new_inner(
        relaxed_re_syntax: bool,
        error_on_slow_pattern: bool,
    ) -> yrx::Compiler<'static> {
        let mut compiler = yrx::Compiler::new();
        compiler.relaxed_re_syntax(relaxed_re_syntax);
        compiler.error_on_slow_pattern(error_on_slow_pattern);
        compiler
    }
}

#[pymethods]
impl Compiler {
    /// Creates a new [`Compiler`].
    ///
    /// The `relaxed_re_syntax` argument controls whether the compiler should
    /// adopt a more relaxed syntax check for regular expressions, allowing
    /// constructs that YARA-X doesn't accept by default.
    ///
    /// YARA-X enforces stricter regular expression syntax compared to YARA.
    /// For instance, YARA accepts invalid escape sequences and treats them
    /// as literal characters (e.g., \R is interpreted as a literal 'R'). It
    /// also allows some special characters to appear unescaped, inferring
    /// their meaning from the context (e.g., `{` and `}` in `/foo{}bar/` are
    /// literal, but in `/foo{0,1}bar/` they form the repetition operator
    /// `{0,1}`).
    ///
    /// The `error_on_slow_pattern` argument tells the compiler to treat slow
    /// patterns as errors, instead of warnings.
    #[new]
    #[pyo3(signature = (relaxed_re_syntax=false, error_on_slow_pattern=false, includes_enabled=true))]
    fn new(
        relaxed_re_syntax: bool,
        error_on_slow_pattern: bool,
        includes_enabled: bool,
    ) -> Self {
        let mut compiler = Self {
            inner: Self::new_inner(relaxed_re_syntax, error_on_slow_pattern),
            relaxed_re_syntax,
            error_on_slow_pattern,
            includes_enabled,
        };
        compiler.inner.enable_includes(includes_enabled);
        compiler
    }

    /// Specify a regular expression that the compiler will enforce upon each
    /// rule name. Any rule which has a name which does not match this regex
    /// will return an InvalidRuleName warning.
    ///
    /// If the regexp does not compile a ValueError is returned.
    #[pyo3(signature = (regexp_str))]
    fn rule_name_regexp(&mut self, regexp_str: &str) -> PyResult<()> {
        let linter = yrx::linters::rule_name(regexp_str)
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        self.inner.add_linter(linter);
        Ok(())
    }

    /// Adds a YARA source code to be compiled.
    ///
    /// This function may be invoked multiple times to add several sets of YARA
    /// rules before calling [`Compiler::build`]. If the rules provided in
    /// `src` contain errors that prevent compilation, the function will raise
    /// an exception with the first error encountered. Additionally, the
    /// compiler will store this error, along with any others discovered during
    /// compilation, which can be accessed using [`Compiler::errors`].
    ///
    /// Even if a previous invocation resulted in a compilation error, you can
    /// continue calling this function. In such cases, any rules that failed to
    /// compile will not be included in the final compiled set.
    ///
    /// The optional parameter `origin` allows to specify the origin of the
    /// source code. This usually receives the path of the file from where the
    /// code was read, but it can be any arbitrary string that conveys information
    /// about the source code's origin.
    #[pyo3(signature = (src, origin=None))]
    fn add_source(
        &mut self,
        src: &str,
        origin: Option<String>,
    ) -> PyResult<()> {
        let mut src = yrx::SourceCode::from(src);

        if let Some(origin) = origin.as_ref() {
            src = src.with_origin(origin)
        }

        self.inner
            .add_source(src)
            .map_err(|err| CompileError::new_err(err.to_string()))?;

        Ok(())
    }

    /// Adds a directory to the list of directories where the compiler should
    /// look for included files.
    ///
    /// When an `include` statement is found, the compiler looks for the included
    /// file in the directories added with this function, in the order they were
    /// added.
    ///
    /// If this function is not called, the compiler will only look for included
    /// files in the current directory.
    ///
    /// Use [Compiler::enable_includes] for controlling whether include statements
    /// are allowed or not.
    ///
    /// # Example
    ///
    /// ```
    /// import yara_x
    /// compiler = yara_x.Compiler()
    /// compiler.add_include_dir("/path/to/rules")
    /// compiler.add_include_dir("/another/path")
    /// ```
    fn add_include_dir(&mut self, dir: &str) {
        self.inner.add_include_dir(dir);
    }

    /// Defines a global variable and sets its initial value.
    ///
    /// Global variables must be defined before calling [`Compiler::add_source`]
    /// with some YARA rule that uses the variable. The variable will retain its
    /// initial value when the [`Rules`] are used for scanning data, however
    /// each scanner can change the variable's value by calling
    /// [`crate::Scanner::set_global`].
    ///
    /// The type of `value` must be: bool, str, bytes, int or float.
    ///
    /// # Raises
    ///
    /// [TypeError](https://docs.python.org/3/library/exceptions.html#TypeError)
    /// if the type of `value` is not one of the supported ones.
    fn define_global(
        &mut self,
        ident: &str,
        value: Bound<PyAny>,
    ) -> PyResult<()> {
        let result = if value.is_exact_instance_of::<PyBool>() {
            self.inner.define_global(ident, value.extract::<bool>()?)
        } else if value.is_exact_instance_of::<PyString>() {
            self.inner.define_global(ident, value.extract::<String>()?)
        } else if value.is_exact_instance_of::<PyBytes>() {
            self.inner.define_global(ident, value.extract::<&[u8]>()?)
        } else if value.is_exact_instance_of::<PyInt>() {
            self.inner.define_global(ident, value.extract::<i64>()?)
        } else if value.is_exact_instance_of::<PyFloat>() {
            self.inner.define_global(ident, value.extract::<f64>()?)
        } else {
            return Err(PyTypeError::new_err(format!(
                "unsupported variable type `{}`",
                value.get_type()
            )));
        };

        result.map_err(|err| PyValueError::new_err(err.to_string()))?;

        Ok(())
    }

    /// Creates a new namespace.
    ///
    /// Further calls to [`Compiler::add_source`] will put the rules under the
    /// newly created namespace.
    fn new_namespace(&mut self, namespace: &str) {
        self.inner.new_namespace(namespace);
    }

    /// Tell the compiler that a YARA module is not supported.
    ///
    /// Import statements for ignored modules will be ignored without errors,
    /// but a warning will be issued. Any rule that makes use of an ignored
    /// module will be also ignored, while the rest of the rules that don't
    /// rely on that module will be correctly compiled.
    fn ignore_module(&mut self, module: &str) {
        self.inner.ignore_module(module);
    }

    /// Enables or disables the inclusion of files with the `include` directive.
    ///
    /// When includes are disabled, any `include` directive encountered in the
    /// source code will be treated as an error. By default, includes are enabled.
    ///
    /// # Example
    ///
    /// ```python
    /// import yara_x
    ///
    /// compiler = yara_x.Compiler()
    /// compiler.enable_includes(False)  # Disable includes
    /// ```
    fn enable_includes(&mut self, yes: bool) {
        self.includes_enabled = yes;
        self.inner.enable_includes(yes);
    }

    /// Builds the source code previously added to the compiler.
    ///
    /// This function returns an instance of [`Rules`] containing all the rules
    /// previously added with [`Compiler::add_source`] and sets the compiler
    /// to its initial empty state.
    fn build(&mut self) -> Rules {
        let compiler = mem::replace(
            &mut self.inner,
            Self::new_inner(
                self.relaxed_re_syntax,
                self.error_on_slow_pattern,
            ),
        );
        Rules::new(compiler.build())
    }

    /// Retrieves all errors generated by the compiler.
    ///
    /// This method returns every error encountered during the compilation,
    /// across all invocations of [`Compiler::add_source`].
    fn errors<'py>(&'py self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let json = PyModule::import(py, "json")?;
        let json_loads = json.getattr("loads")?;
        let errors_json = serde_json::to_string_pretty(&self.inner.errors());
        let errors_json = errors_json
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        json_loads.call((errors_json,), None)
    }

    /// Retrieves all warnings generated by the compiler.
    ///
    /// This method returns every warning encountered during the compilation,
    /// across all invocations of [`Compiler::add_source`].
    fn warnings<'py>(
        &'py self,
        py: Python<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let json = PyModule::import(py, "json")?;
        let json_loads = json.getattr("loads")?;
        let warnings_json =
            serde_json::to_string_pretty(&self.inner.warnings());
        let warnings_json = warnings_json
            .map_err(|err| PyValueError::new_err(err.to_string()))?;
        json_loads.call((warnings_json,), None)
    }
}

/// Scans data with already compiled YARA rules.
///
/// The scanner receives a set of compiled Rules and scans data with those
/// rules. The same scanner can be used for scanning multiple files or
/// in-memory data sequentially, but you need multiple scanners for scanning
/// in parallel.
#[pyclass(unsendable)]
struct Scanner {
    // The only purpose of this field is making sure that the `Rules` object
    // is not freed while the `Scanner` object is still around. This reference
    // to the `Rules` object will keep it alive during the scanner lifetime.
    //
    // We need the `Rules` object alive because the `yrx::Scanner` holds a
    // reference to the `yrx::Rules` contained in `Rules`. This reference
    // is obtained in an unsafe manner from a pointer, for that reason the
    // `yrx::Rules` are pinned, so that they are not moved from their
    // original location and the reference remains valid.
    _rules: Py<Rules>,
    inner: yrx::Scanner<'static>,
}

#[pymethods]
impl Scanner {
    /// Creates a new [`Scanner`] with a given set of [`Rules`].
    #[new]
    fn new(rules: Py<Rules>) -> Self {
        Python::with_gil(|py| {
            let rules_ref: &'static yrx::Rules = {
                let rules = rules.borrow(py);
                let rules_ptr: *const yrx::Rules = &rules.deref().inner.rules;
                unsafe { &*rules_ptr }
            };
            Self { _rules: rules, inner: yrx::Scanner::new(rules_ref) }
        })
    }

    /// Sets the value of a global variable.
    ///
    /// The variable must has been previously defined by calling
    /// [`Compiler::define_global`], and the type it has during the definition
    /// must match the type of the new value.
    ///
    /// The variable will retain the new value in subsequent scans, unless this
    /// function is called again for setting a new value.
    ///
    /// The type of `value` must be: `bool`, `str`, `bytes`, `int` or `float`.
    ///
    /// # Raises
    ///
    /// [TypeError](https://docs.python.org/3/library/exceptions.html#TypeError)
    /// if the type of `value` is not one of the supported ones.
    fn set_global(
        &mut self,
        ident: &str,
        value: Bound<PyAny>,
    ) -> PyResult<()> {
        let result = if value.is_exact_instance_of::<PyBool>() {
            self.inner.set_global(ident, value.extract::<bool>()?)
        } else if value.is_exact_instance_of::<PyString>() {
            self.inner.set_global(ident, value.extract::<String>()?)
        } else if value.is_exact_instance_of::<PyBytes>() {
            self.inner.set_global(ident, value.extract::<&[u8]>()?)
        } else if value.is_exact_instance_of::<PyInt>() {
            self.inner.set_global(ident, value.extract::<i64>()?)
        } else if value.is_exact_instance_of::<PyFloat>() {
            self.inner.set_global(ident, value.extract::<f64>()?)
        } else {
            return Err(PyTypeError::new_err(format!(
                "unsupported variable type `{}`",
                value.get_type()
            )));
        };

        result.map_err(|err| PyValueError::new_err(err.to_string()))?;

        Ok(())
    }

    /// Sets a timeout for each scan.
    ///
    /// After setting a timeout scans will abort after the specified `seconds`.
    fn set_timeout(&mut self, seconds: u64) {
        self.inner.set_timeout(Duration::from_secs(seconds));
    }

    /// Sets a callback that is invoked every time a YARA rule calls the
    /// `console` module.
    ///
    /// The `callback` function is invoked with a string representing the
    /// message being logged. The function can print the message to stdout,
    /// append it to a file, etc. If no callback is set these messages are
    /// ignored.
    fn console_log(&mut self, callback: PyObject) -> PyResult<()> {
        if !Python::with_gil(|py| callback.bind(py).is_callable()) {
            return Err(PyValueError::new_err("callback is not callable"));
        }
        self.inner.console_log(move |msg| {
            let _ = Python::with_gil(|py| -> PyResult<PyObject> {
                callback.call1(py, (msg,))
            });
        });
        Ok(())
    }

    /// Scans in-memory data.
    fn scan(&mut self, data: &[u8]) -> PyResult<Py<ScanResults>> {
        Python::with_gil(|py| {
            scan_results_to_py(
                py,
                self.inner.scan(data).map_err(map_scan_err)?,
            )
        })
    }

    /// Scans a file.
    fn scan_file(&mut self, path: PathBuf) -> PyResult<Py<ScanResults>> {
        Python::with_gil(|py| {
            scan_results_to_py(
                py,
                self.inner.scan_file(path).map_err(map_scan_err)?,
            )
        })
    }
}

/// Results produced by a scan operation.
#[pyclass]
struct ScanResults {
    /// Vector that contains all the rules that matched during the scan.
    matching_rules: Py<PyTuple>,
    /// Dictionary where keys are module names and values are other
    /// dictionaries with the information produced by the corresponding module.
    module_outputs: Py<PyDict>,
}

#[pymethods]
impl ScanResults {
    #[getter]
    /// Rules that matched during the scan.
    fn matching_rules(&self) -> Py<PyTuple> {
        Python::with_gil(|py| self.matching_rules.clone_ref(py))
    }

    #[getter]
    /// Module output from the scan.
    fn module_outputs<'py>(
        &'py self,
        py: Python<'py>,
    ) -> &'py Bound<'py, PyDict> {
        self.module_outputs.bind(py)
    }
}

/// Represents a rule that matched while scanning some data.
#[pyclass]
struct Rule {
    identifier: String,
    namespace: String,
    tags: Py<PyTuple>,
    metadata: Py<PyTuple>,
    patterns: Py<PyTuple>,
}

#[pymethods]
impl Rule {
    #[getter]
    /// Returns the rule's name.
    fn identifier(&self) -> &str {
        self.identifier.as_str()
    }

    /// Returns the rule's namespace.
    #[getter]
    fn namespace(&self) -> &str {
        self.namespace.as_str()
    }

    /// Returns the rule's tags.
    #[getter]
    fn tags(&self) -> Py<PyTuple> {
        Python::with_gil(|py| self.tags.clone_ref(py))
    }

    /// A tuple of pairs `(identifier, value)` with the metadata associated to
    /// the rule.
    #[getter]
    fn metadata(&self) -> Py<PyTuple> {
        Python::with_gil(|py| self.metadata.clone_ref(py))
    }

    /// Patterns defined by the rule.
    #[getter]
    fn patterns(&self) -> Py<PyTuple> {
        Python::with_gil(|py| self.patterns.clone_ref(py))
    }
}

/// Represents a pattern in a YARA rule.
#[pyclass]
struct Pattern {
    identifier: String,
    matches: Py<PyTuple>,
}

#[pymethods]
impl Pattern {
    /// Pattern identifier (e.g: '$a', '$foo').
    #[getter]
    fn identifier(&self) -> &str {
        self.identifier.as_str()
    }

    /// Matches found for this pattern.
    #[getter]
    fn matches(&self) -> Py<PyTuple> {
        Python::with_gil(|py| self.matches.clone_ref(py))
    }
}

/// Represents a match found for a pattern.
#[pyclass]
struct Match {
    /// Offset within the scanned data where the match occurred.
    offset: usize,
    /// Length of the match.
    length: usize,
    /// For patterns that have the `xor` modifier, contains the XOR key that
    /// applied to matching data. For any other pattern will be `None`.
    xor_key: Option<u8>,
}

#[pymethods]
impl Match {
    /// Offset where the match occurred.
    #[getter]
    fn offset(&self) -> usize {
        self.offset
    }

    /// Length of the match in bytes.
    #[getter]
    fn length(&self) -> usize {
        self.length
    }

    /// XOR key used for decrypting the data if the pattern had the xor
    /// modifier, or None if otherwise.
    #[getter]
    fn xor_key(&self) -> Option<u8> {
        self.xor_key
    }
}

/// A set of YARA rules in compiled form.
///
/// This is the result of [`Compiler::build`].
#[pyclass]
struct Rules {
    inner: Pin<Box<PinnedRules>>,
}

struct PinnedRules {
    rules: yrx::Rules,
    _pinned: PhantomPinned,
}

impl Rules {
    fn new(rules: yrx::Rules) -> Self {
        Rules {
            inner: Box::pin(PinnedRules { rules, _pinned: PhantomPinned }),
        }
    }
}

#[pymethods]
impl Rules {
    /// Scans in-memory data with these rules.
    fn scan(&self, data: &[u8]) -> PyResult<Py<ScanResults>> {
        let mut scanner = yrx::Scanner::new(&self.inner.rules);
        Python::with_gil(|py| {
            scan_results_to_py(
                py,
                scanner
                    .scan(data)
                    .map_err(|err| ScanError::new_err(err.to_string()))?,
            )
        })
    }

    /// Serializes the rules into a file-like object.
    fn serialize_into(&self, file: PyObject) -> PyResult<()> {
        let f = PyFileLikeObject::with_requirements(
            file, false, true, false, false,
        )?;
        self.inner
            .rules
            .serialize_into(f)
            .map_err(|err| PyIOError::new_err(err.to_string()))
    }

    /// Deserializes rules from a file-like object.
    #[staticmethod]
    fn deserialize_from(file: PyObject) -> PyResult<Py<Rules>> {
        let f = PyFileLikeObject::with_requirements(
            file, true, false, false, false,
        )?;
        let rules = yrx::Rules::deserialize_from(f)
            .map_err(|err| PyIOError::new_err(err.to_string()))?;

        Python::with_gil(|py| Py::new(py, Rules::new(rules)))
    }
}

fn scan_results_to_py(
    py: Python,
    scan_results: yrx::ScanResults,
) -> PyResult<Py<ScanResults>> {
    let matching_rules = scan_results
        .matching_rules()
        .map(|rule| rule_to_py(py, rule))
        .collect::<PyResult<Vec<_>>>()?;

    let module_outputs = PyDict::new(py);
    let outputs = scan_results.module_outputs();

    if outputs.len() > 0 {
        for (module, output) in outputs {
            module_outputs.set_item(module, proto_to_json(py, output)?)?;
        }
    }

    Py::new(
        py,
        ScanResults {
            matching_rules: PyTuple::new(py, matching_rules)?.unbind(),
            module_outputs: module_outputs.into(),
        },
    )
}

fn rule_to_py(py: Python, rule: yrx::Rule) -> PyResult<Py<Rule>> {
    Py::new(
        py,
        Rule {
            identifier: rule.identifier().to_string(),
            namespace: rule.namespace().to_string(),
            tags: PyTuple::new(py, rule.tags().map(|tag| tag.identifier()))?
                .unbind(),
            metadata: PyTuple::new(
                py,
                rule.metadata()
                    .map(|(ident, value)| metadata_to_py(py, ident, value)),
            )?
            .unbind(),
            patterns: PyTuple::new(
                py,
                rule.patterns()
                    .map(|pattern| pattern_to_py(py, pattern))
                    .collect::<Result<Vec<_>, _>>()?,
            )?
            .unbind(),
        },
    )
}

fn metadata_to_py(
    py: Python,
    ident: &str,
    metadata: yrx::MetaValue,
) -> Py<PyTuple> {
    let value = match metadata {
        yrx::MetaValue::Integer(v) => v.into_py_any(py),
        yrx::MetaValue::Float(v) => v.into_py_any(py),
        yrx::MetaValue::Bool(v) => v.into_py_any(py),
        yrx::MetaValue::String(v) => v.into_py_any(py),
        yrx::MetaValue::Bytes(v) => v.into_py_any(py),
    }
    .unwrap();

    PyTuple::new(py, [ident.into_py_any(py).unwrap(), value]).unwrap().unbind()
}

fn pattern_to_py(py: Python, pattern: yrx::Pattern) -> PyResult<Py<Pattern>> {
    Py::new(
        py,
        Pattern {
            identifier: pattern.identifier().to_string(),
            matches: PyTuple::new(
                py,
                pattern
                    .matches()
                    .map(|match_| match_to_py(py, match_))
                    .collect::<Result<Vec<_>, _>>()?,
            )?
            .unbind(),
        },
    )
}

fn match_to_py(py: Python, match_: yrx::Match) -> PyResult<Py<Match>> {
    Py::new(
        py,
        Match {
            offset: match_.range().start,
            length: match_.range().len(),
            xor_key: match_.xor_key(),
        },
    )
}

/// Decodes the JSON output generated by YARA modules and converts it
/// into a native Python dictionary.
///
/// YARA module outputs often include values that require special handling.
/// In particular, raw byte strings—since they cannot be directly represented
/// in JSON—are encoded as base64 and wrapped in an object that includes
/// both the encoded value and metadata about the encoding. For example:
///
/// ```json
/// "my_bytes_field": {
///   "encoding": "base64",
///   "value": "dGhpcyBpcyB0aGUgb3JpZ2luYWwgdmFsdWU="
/// }
/// ```
///
/// This decoder identifies such structures, decodes the base64-encoded content,
/// and returns the result as a Python `bytes` object, preserving the original
/// binary data.
#[pyclass]
struct JsonDecoder {
    fromtimestamp: Py<PyAny>,
}

#[pymethods]
impl JsonDecoder {
    #[staticmethod]
    fn new() -> Self {
        JsonDecoder {
            fromtimestamp: Python::with_gil(|py| {
                PyModule::import(py, "datetime")
                    .unwrap()
                    .getattr("datetime")
                    .unwrap()
                    .getattr("fromtimestamp")
                    .unwrap()
                    .unbind()
            }),
        }
    }

    fn __call__<'py>(
        &self,
        py: Python<'py>,
        dict: Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(encoding) = dict
            .get_item("encoding")?
            .as_ref()
            .and_then(|encoding| encoding.downcast::<PyString>().ok())
        {
            let value = match dict.get_item("value")? {
                Some(value) => value,
                None => return Ok(dict.into_any()),
            };

            if encoding == "base64" {
                BASE64_STANDARD
                    .decode(value.downcast::<PyString>()?.to_cow()?.as_bytes())
                    .expect("decoding base64")
                    .into_bound_py_any(py)
            } else if encoding == "timestamp" {
                let kwargs = PyDict::new(py);
                kwargs.set_item("tz", PyTzInfo::utc(py)?)?;
                self.fromtimestamp
                    .call(py, (value,), Some(&kwargs))?
                    .into_bound_py_any(py)
            } else {
                Ok(dict.into_any())
            }
        } else {
            Ok(dict.into_any())
        }
    }
}

fn proto_to_json<'py>(
    py: Python<'py>,
    proto: &dyn MessageDyn,
) -> PyResult<Bound<'py, PyAny>> {
    let mut module_output_json = Vec::new();

    let mut serializer =
        yara_x_proto_json::Serializer::new(&mut module_output_json);

    serializer
        .serialize(proto)
        .expect("unable to serialize JSON produced by module");

    let json = PyModule::import(py, "json")?;
    let json_loads = json.getattr("loads")?;

    let kwargs = PyDict::new(py);

    // The `object_hook` argument for `json.loads` allows to pass a callable
    // that can transform JSON objects on the fly. This is used in order to
    // decode some types that are not directly representable in JSON. See the
    // documentation for JsonDecode for details.
    kwargs.set_item("object_hook", JsonDecoder::new())?;
    // By default, json.loads doesn't allow control character (\t, \n, etc)
    // in strings, we need to set strict=False to allow them.
    // https://github.com/VirusTotal/yara-x/issues/365
    kwargs.set_item("strict", false)?;

    json_loads.call((module_output_json,), Some(&kwargs))
}

create_exception!(
    yara_x,
    CompileError,
    PyException,
    "Exception raised when compilation fails"
);

create_exception!(
    yara_x,
    TimeoutError,
    PyException,
    "Exception raised when a timeout occurs during a scan"
);

create_exception!(
    yara_x,
    ScanError,
    PyException,
    "Exception raised when scanning fails"
);

fn map_scan_err(err: yrx::errors::ScanError) -> PyErr {
    match err {
        yrx::errors::ScanError::Timeout => TimeoutError::new_err("timeout"),
        err => ScanError::new_err(err.to_string()),
    }
}

/// Python module for compiling YARA rules and scanning data with them.
///
/// Usage:
///
/// >>> import yara_x
/// >>> rules = yara_x.compile('rule test {strings: $a = "dummy" condition: $a}')
/// >>> matches = rules.scan(b'some dummy data')
/// ```
#[pymodule]
fn yara_x(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CompileError", m.py().get_type::<CompileError>())?;
    m.add("TimeoutError", m.py().get_type::<TimeoutError>())?;
    m.add("ScanError", m.py().get_type::<ScanError>())?;
    m.add_function(wrap_pyfunction!(compile, m)?)?;
    m.add_class::<Rules>()?;
    m.add_class::<Scanner>()?;
    m.add_class::<Compiler>()?;
    m.add_class::<Rule>()?;
    m.add_class::<Pattern>()?;
    m.add_class::<Match>()?;
    m.add_class::<Formatter>()?;
    m.add_class::<Module>()?;
    m.add_class::<JsonDecoder>()?;
    Ok(())
}
