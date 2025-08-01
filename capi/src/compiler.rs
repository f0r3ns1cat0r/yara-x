use std::ffi::{c_char, CStr};
use std::mem;
use std::mem::ManuallyDrop;

use yara_x::errors::{CompileError, SerializationError, VariableError};
use yara_x::SourceCode;

use crate::{_yrx_set_last_error, YRX_BUFFER, YRX_RESULT, YRX_RULES};

/// A compiler that takes YARA source code and produces compiled rules.
pub struct YRX_COMPILER<'a> {
    inner: yara_x::Compiler<'a>,
    flags: u32,
}

/// Flag passed to [`yrx_compiler_create`] for producing colorful error
/// messages.
pub const YRX_COLORIZE_ERRORS: u32 = 1;

/// Flag passed to [`yrx_compiler_create`] that enables a more relaxed
/// syntax check for regular expressions.
///
/// YARA-X enforces stricter regular expression syntax compared to YARA.
/// For instance, YARA accepts invalid escape sequences and treats them
/// as literal characters (e.g., \R is interpreted as a literal 'R'). It
/// also allows some special characters to appear unescaped, inferring
/// their meaning from the context (e.g., `{` and `}` in `/foo{}bar/` are
/// literal, but in `/foo{0,1}bar/` they form the repetition operator
/// `{0,1}`).
///
/// When this flag is set, YARA-X mimics YARA's behavior, allowing
/// constructs that YARA-X doesn't accept by default.
pub const YRX_RELAXED_RE_SYNTAX: u32 = 2;

/// Flag passed to [`yrx_compiler_create`] for treating slow patterns as
/// errors instead of warnings.
pub const YRX_ERROR_ON_SLOW_PATTERN: u32 = 4;

/// Flag passed to [`yrx_compiler_create`] for treating slow loops as
/// errors instead of warnings.
pub const YRX_ERROR_ON_SLOW_LOOP: u32 = 8;

/// Flag passed to [`yrx_compiler_create`] for enabling optimizations.
/// With this flag the compiler tries to optimize rule conditions by applying
/// techniques like common subexpression elimination (CSE) and loop-invariant
/// code motion (LICM).
pub const YRX_ENABLE_CONDITION_OPTIMIZATION: u32 = 16;

/// Flag passed to [`yrx_compiler_create`] for disabling `include` statements.
/// With this flag, the compiler produces an error when `include` statements are
/// encountered.
pub const YRX_DISABLE_INCLUDES: u32 = 32;

fn _yrx_compiler_create<'a>(flags: u32) -> yara_x::Compiler<'a> {
    let mut compiler = yara_x::Compiler::new();
    if flags & YRX_RELAXED_RE_SYNTAX != 0 {
        compiler.relaxed_re_syntax(true);
    }
    if flags & YRX_ENABLE_CONDITION_OPTIMIZATION != 0 {
        compiler.condition_optimization(true);
    }
    if flags & YRX_COLORIZE_ERRORS != 0 {
        compiler.colorize_errors(true);
    }
    if flags & YRX_ERROR_ON_SLOW_PATTERN != 0 {
        compiler.error_on_slow_pattern(true);
    }
    if flags & YRX_ERROR_ON_SLOW_LOOP != 0 {
        compiler.error_on_slow_loop(true);
    }
    if flags & YRX_DISABLE_INCLUDES != 0 {
        compiler.enable_includes(false);
    }
    compiler
}

/// Creates a [`YRX_COMPILER`] object.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_create(
    flags: u32,
    compiler: &mut *mut YRX_COMPILER,
) -> YRX_RESULT {
    *compiler = Box::into_raw(Box::new(YRX_COMPILER {
        inner: _yrx_compiler_create(flags),
        flags,
    }));

    YRX_RESULT::YRX_SUCCESS
}

/// Destroys a [`YRX_COMPILER`] object.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_destroy(compiler: *mut YRX_COMPILER) {
    drop(Box::from_raw(compiler))
}

/// Adds a YARA source code to be compiled.
///
/// This function can be called multiple times.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_add_source(
    compiler: *mut YRX_COMPILER,
    src: *const c_char,
) -> YRX_RESULT {
    yrx_compiler_add_source_with_origin(compiler, src, std::ptr::null())
}

/// Adds a YARA source code to be compiled, specifying an origin for the
/// source code.
///
/// This function is similar to [`yrx_compiler_add_source`], but in addition
/// to the source code itself it provides a string that identifies the origin
/// of the code, usually the file path from where the source was obtained.
///
/// This origin is shown in error reports.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_add_source_with_origin(
    compiler: *mut YRX_COMPILER,
    src: *const c_char,
    origin: *const c_char,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let src = CStr::from_ptr(src);
    let mut src = SourceCode::from(src.to_bytes());

    if !origin.is_null() {
        let origin = CStr::from_ptr(origin);
        src = match origin.to_str() {
            Ok(origin) => src.with_origin(origin),
            Err(_) => return YRX_RESULT::YRX_INVALID_ARGUMENT,
        };
    }

    match compiler.inner.add_source(src) {
        Ok(_) => {
            _yrx_set_last_error::<CompileError>(None);
            YRX_RESULT::YRX_SUCCESS
        }
        Err(err) => {
            _yrx_set_last_error(Some(err));
            YRX_RESULT::YRX_SYNTAX_ERROR
        }
    }
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
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_add_include_dir(
    compiler: *mut YRX_COMPILER,
    dir: *const c_char,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let dir = if let Ok(dir) = CStr::from_ptr(dir).to_str() {
        dir
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    compiler.inner.add_include_dir(dir);

    YRX_RESULT::YRX_SUCCESS
}

/// Tell the compiler that a YARA module is not supported.
///
/// Import statements for ignored modules will be ignored without errors but a
/// warning will be issued. Any rule that make use of an ignored module will be
/// ignored, while the rest of rules that don't rely on that module will be
/// correctly compiled.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_ignore_module(
    compiler: *mut YRX_COMPILER,
    module: *const c_char,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let module = if let Ok(module) = CStr::from_ptr(module).to_str() {
        module
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    compiler.inner.ignore_module(module);

    YRX_RESULT::YRX_SUCCESS
}

/// Enables a feature on this compiler.
///
/// When defining the structure of a module in a `.proto` file, you can
/// specify that certain fields are accessible only when one or more
/// features are enabled. For example, the snippet below shows the
/// definition of a field named `requires_foo_and_bar`, which can be
/// accessed only when both features "foo" and "bar" are enabled.
///
/// ```protobuf
/// optional uint64 requires_foo_and_bar = 500 [
///   (yara.field_options) = {
///     acl: [
///       {
///         allow_if: "foo",
///         error_title: "foo is required",
///         error_label: "this field was used without foo"
///       },
///       {
///         allow_if: "bar",
///         error_title: "bar is required",
///         error_label: "this field was used without bar"
///       }
///     ]
///   }
/// ];
/// ```
///
/// If some of the required features are not enabled, using this field in
/// a YARA rule will cause an error while compiling the rules. The error
/// looks like:
///
/// ```text
/// error[E034]: foo is required
///  --> line:5:29
///   |
/// 5 |  test_proto2.requires_foo_and_bar == 0
///   |              ^^^^^^^^^^^^^^^^^^^^ this field was used without foo
///   |
/// ```
///
/// Notice that both the title and label in the error message are defined
/// in the .proto file.
///
/// # Important
///
/// This API is hidden from the public documentation because it is unstable
/// and subject to change.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_enable_feature(
    compiler: *mut YRX_COMPILER,
    feature: *const c_char,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let feature = if let Ok(module) = CStr::from_ptr(feature).to_str() {
        module
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    compiler.inner.enable_feature(feature);

    YRX_RESULT::YRX_SUCCESS
}

/// Tell the compiler that a YARA module can't be used.
///
/// Import statements for the banned module will cause an error. The error
/// message can be customized by using the given error title and message.
///
/// If this function is called multiple times with the same module name,
/// the error title and message will be updated.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_ban_module(
    compiler: *mut YRX_COMPILER,
    module: *const c_char,
    error_title: *const c_char,
    error_msg: *const c_char,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let module = if let Ok(module) = CStr::from_ptr(module).to_str() {
        module
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let err_title = if let Ok(err_title) = CStr::from_ptr(error_title).to_str()
    {
        err_title
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let err_msg = if let Ok(err_msg) = CStr::from_ptr(error_msg).to_str() {
        err_msg
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    compiler.inner.ban_module(module, err_title, err_msg);

    YRX_RESULT::YRX_SUCCESS
}

/// Creates a new namespace.
///
/// Further calls to `yrx_compiler_add_source` will put the rules under the
/// newly created namespace.
///
/// The `namespace` argument must be pointer to null-terminated UTF-8 string.
/// If the string is not valid UTF-8 the result is an `INVALID_ARGUMENT` error.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_new_namespace(
    compiler: *mut YRX_COMPILER,
    namespace: *const c_char,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let namespace = if let Ok(namespace) = CStr::from_ptr(namespace).to_str() {
        namespace
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    compiler.inner.new_namespace(namespace);

    YRX_RESULT::YRX_SUCCESS
}

/// Defines a global variable and sets its initial value.
///
/// Global variables must be defined before using `yrx_compiler_add_source`
/// for adding any YARA source code that uses those variables. The variable
/// will retain its initial value when the compiled rules are used for
/// scanning data, however each scanner can change the variable’s initial
/// value by calling `yrx_scanner_set_global`.
unsafe fn yrx_compiler_define_global<
    T: TryInto<yara_x::Variable, Error = yara_x::errors::VariableError>,
>(
    compiler: *mut YRX_COMPILER,
    ident: *const c_char,
    value: T,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    let ident = if let Ok(ident) = CStr::from_ptr(ident).to_str() {
        ident
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    match compiler.inner.define_global(ident, value) {
        Ok(_) => {
            _yrx_set_last_error::<VariableError>(None);
            YRX_RESULT::YRX_SUCCESS
        }
        Err(err) => {
            _yrx_set_last_error(Some(err));
            YRX_RESULT::YRX_VARIABLE_ERROR
        }
    }
}

/// Defines a global variable of string type and sets its initial value.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_define_global_str(
    compiler: *mut YRX_COMPILER,
    ident: *const c_char,
    value: *const c_char,
) -> YRX_RESULT {
    let value = if let Ok(value) = CStr::from_ptr(value).to_str() {
        value
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    yrx_compiler_define_global(compiler, ident, value)
}

/// Defines a global variable of bool type and sets its initial value.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_define_global_bool(
    compiler: *mut YRX_COMPILER,
    ident: *const c_char,
    value: bool,
) -> YRX_RESULT {
    yrx_compiler_define_global(compiler, ident, value)
}

/// Defines a global variable of integer type and sets its initial value.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_define_global_int(
    compiler: *mut YRX_COMPILER,
    ident: *const c_char,
    value: i64,
) -> YRX_RESULT {
    yrx_compiler_define_global(compiler, ident, value)
}

/// Defines a global variable of float type and sets its initial value.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_define_global_float(
    compiler: *mut YRX_COMPILER,
    ident: *const c_char,
    value: f64,
) -> YRX_RESULT {
    yrx_compiler_define_global(compiler, ident, value)
}

/// Returns the errors encountered during the compilation in JSON format.
///
/// In the address indicated by the `buf` pointer, the function will copy a
/// `YRX_BUFFER*` pointer. The `YRX_BUFFER` structure represents a buffer
/// that contains the JSON representation of the compilation errors.
///
/// The JSON consists on an array of objects, each object representing a
/// compilation error. The object has the following fields:
///
/// * type: A string that describes the type of error.
/// * code: Error code (e.g: "E009").
/// * title: Error title (e.g: "unknown identifier `foo`").
/// * labels: Array of labels.
/// * text: The full text of the error report, as shown by the command-line tool.
///
/// Here is an example:
///
/// ```json
/// [
///     {
///         "type": "UnknownIdentifier",
///         "code": "E009",
///         "title": "unknown identifier `foo`",
///         "labels": [
///             {
///                 "level": "error",
///                 "code_origin": null,
///                 "span": {"start":25,"end":28},
///                 "text": "this identifier has not been declared"
///             }
///         ],
///         "text": "... <full report here> ..."
///     }
/// ]
/// ```
///
/// The [`YRX_BUFFER`] must be destroyed with [`yrx_buffer_destroy`].
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_errors_json(
    compiler: *mut YRX_COMPILER,
    buf: &mut *mut YRX_BUFFER,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    match serde_json::to_vec(compiler.inner.errors()) {
        Ok(json) => {
            let json = json.into_boxed_slice();
            let mut json = ManuallyDrop::new(json);
            *buf = Box::into_raw(Box::new(YRX_BUFFER {
                data: json.as_mut_ptr(),
                length: json.len(),
            }));
            _yrx_set_last_error::<SerializationError>(None);
            YRX_RESULT::YRX_SUCCESS
        }
        Err(err) => {
            _yrx_set_last_error(Some(err));
            YRX_RESULT::YRX_SERIALIZATION_ERROR
        }
    }
}

/// Returns the warnings encountered during the compilation in JSON format.
///
/// In the address indicated by the `buf` pointer, the function will copy a
/// `YRX_BUFFER*` pointer. The `YRX_BUFFER` structure represents a buffer
/// that contains the JSON representation of the warnings.
///
/// The JSON consists on an array of objects, each object representing a
/// warning. The object has the following fields:
///
/// * type: A string that describes the type of warning.
/// * code: Warning code (e.g: "slow_pattern").
/// * title: Error title (e.g: "slow pattern").
/// * labels: Array of labels.
/// * text: The full text of the warning report, as shown by the command-line tool.
///
/// Here is an example:
///
/// ```json
/// [
///     {
///         "type": "SlowPattern",
///         "code": "slow_pattern",
///         "title": "slow pattern",
///         "labels": [
///             {
///                 "level": "warning",
///                 "code_origin": null,
///                 "span": {"start":25,"end":28},
///                 "text": "this pattern may slow down the scan"
///             }
///         ],
///         "text": "... <full report here> ..."
///     }
/// ]
/// ```
///
/// The [`YRX_BUFFER`] must be destroyed with [`yrx_buffer_destroy`].
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_warnings_json(
    compiler: *mut YRX_COMPILER,
    buf: &mut *mut YRX_BUFFER,
) -> YRX_RESULT {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return YRX_RESULT::YRX_INVALID_ARGUMENT;
    };

    match serde_json::to_vec(compiler.inner.warnings()) {
        Ok(json) => {
            let json = json.into_boxed_slice();
            let mut json = ManuallyDrop::new(json);
            *buf = Box::into_raw(Box::new(YRX_BUFFER {
                data: json.as_mut_ptr(),
                length: json.len(),
            }));
            _yrx_set_last_error::<SerializationError>(None);
            YRX_RESULT::YRX_SUCCESS
        }
        Err(err) => {
            _yrx_set_last_error(Some(err));
            YRX_RESULT::YRX_SERIALIZATION_ERROR
        }
    }
}

/// Builds the source code previously added to the compiler.
///
/// After calling this function the compiler is reset to its initial state,
/// (i.e: the state it had after returning from yrx_compiler_create) you can
/// keep using it by adding more sources and calling this function again.
#[no_mangle]
pub unsafe extern "C" fn yrx_compiler_build(
    compiler: *mut YRX_COMPILER,
) -> *mut YRX_RULES {
    let compiler = if let Some(compiler) = compiler.as_mut() {
        compiler
    } else {
        return std::ptr::null_mut();
    };

    // As the build() method consumes the compiler, we need to take ownership
    // of it, but that implies that the inner compiler in the YRX_COMPILER
    // object must be replaced with something else, either a null value or a
    // new compiler.It is replaced with a new compiler, so that users of the
    // C API can keep using the YRX_COMPILER object after calling
    // yrx_compiler_build.
    let compiler = mem::replace(
        &mut compiler.inner,
        _yrx_compiler_create(compiler.flags),
    );

    Box::into_raw(YRX_RULES::boxed(compiler.build()))
}
