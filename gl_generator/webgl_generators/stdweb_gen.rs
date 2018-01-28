use std::io;
use std::collections::BTreeSet;

use webgl_registry::*;

#[allow(missing_copy_implementations)]
#[derive(Debug)]
pub struct StdwebGenerator;

#[derive(Clone, Debug)]
struct GenericContext {
    args: BTreeSet<String>,
    constraints: Vec<String>,
}

impl GenericContext {
    pub fn new() -> Self {
        GenericContext {
            args: BTreeSet::new(),
            constraints: Vec::new(),
        }
    }
    pub fn arg(&mut self, desired_name: &str) -> String {
        for i in 0.. {
            let name = format!("{}{}", desired_name, i);
            if !self.args.contains(&name) {
                self.args.insert(name.clone());
                return name;
            }
        }
        unreachable!()
    }
    pub fn constrain(&mut self, constraint: String) {
        self.constraints.push(constraint);
    }
    pub fn args(&self) -> String {
        if self.args.is_empty() {
            String::new()
        } else {
            let args: Vec<_> = self.args.iter().cloned().collect();
            format!("<{}>", args.join(", "))
        }
    }
    pub fn constraints(&self) -> String {
        if self.constraints.is_empty() {
            String::new()
        } else {
            format!(" where {}", self.constraints.join(", "))
        }
    }
}

#[derive(Clone, Debug)]
enum ArgWrapper {
    None,
    AsTypedArray,
    Optional(Box<ArgWrapper>),
    Sequence(Box<ArgWrapper>),
    DoubleCast,
}

impl ArgWrapper {
    fn wrap(&self, arg: &str) -> String {
        match self {
            &ArgWrapper::None => arg.into(),
            &ArgWrapper::AsTypedArray => format!("{}.as_typed_array()", arg),
            &ArgWrapper::Optional(ref inner) => format!("{}.map(|inner| {})", arg, inner.wrap("inner")),
            &ArgWrapper::Sequence(ref inner) => format!("{}.iter().map(|inner| {}).collect::<Vec<_>>()", arg, inner.wrap("inner")),
            &ArgWrapper::DoubleCast => format!("({} as f64)", arg),
        }
    }
}

#[derive(Clone, Debug)]
struct ProcessedArg {
    type_: String,
    wrapper: ArgWrapper,
    optional: bool,
}

impl ProcessedArg {
    fn simple<S: Into<String>>(name: S) -> ProcessedArg {
        ProcessedArg { type_: name.into(), wrapper: ArgWrapper::None, optional: false }
    }
}

fn process_arg_type_kind(name: &str, type_kind: &TypeKind, registry: &Registry, gc: &mut GenericContext) -> ProcessedArg {
    let flat_kind = type_kind.flatten(registry);
    match flat_kind {
        &TypeKind::Primitive(ref p) => {
            match p {
                &Primitive::I64 => ProcessedArg { type_: "i64".into(), wrapper: ArgWrapper::DoubleCast, optional: false },
                &Primitive::U64 => ProcessedArg { type_: "u64".into(), wrapper: ArgWrapper::DoubleCast, optional: false },
                other => ProcessedArg::simple(other.name())
            }
        },
        &TypeKind::String => ProcessedArg::simple("&str"),
        &TypeKind::ArrayBuffer | &TypeKind::ArrayBufferView => ProcessedArg::simple("&ArrayBuffer"),
        &TypeKind::BufferSource => ProcessedArg::simple("&ArrayBuffer"),
        &TypeKind::CanvasElement => ProcessedArg::simple("&CanvasElement"),
        &TypeKind::TypedArray(ref p) => {
            let lt = gc.arg("'a");
            let gp = gc.arg("T");
            gc.constrain(format!("{}: AsTypedArray<{}, {}>", gp, lt, p.name()));
            ProcessedArg { type_: gp, wrapper: ArgWrapper::AsTypedArray, optional: false }
        },
        &TypeKind::Sequence(ref t) => {
            let inner = process_arg_type(t, registry, gc);
            ProcessedArg {
                type_: format!("&[{}]", inner.type_),
                wrapper: match inner.wrapper {
                    ArgWrapper::None => ArgWrapper::None,
                    other => ArgWrapper::Sequence(Box::new(other))
                },
                optional: false
            }
        },
        &TypeKind::Dictionary | &TypeKind::Interface => ProcessedArg::simple(format!("&{}", name)),
        &TypeKind::Enum => ProcessedArg::simple(name),
        &TypeKind::Typedef(ref t) => {
            // We have to "look through" the typedef, as the correct parameter
            // type is not representable using the alias.
            assert!(t.optional);
            process_arg_type(t, registry, gc)
        },
        &TypeKind::Any | &TypeKind::Object => {
            let gp = gc.arg("T");
            gc.constrain(format!("{}: JsSerializable", gp));
            ProcessedArg::simple(gp)
        }
    }
}

fn process_arg_type(type_: &Type, registry: &Registry, gc: &mut GenericContext) -> ProcessedArg {
    let type_kind = registry.resolve_type(&type_.name);
    let mut result = process_arg_type_kind(&type_.name, &type_kind, registry, gc);
    if type_.optional && !result.optional {
        result.type_ = format!("Option<{}>", result.type_);
        result.wrapper = match result.wrapper {
            ArgWrapper::None => ArgWrapper::None,
            other => ArgWrapper::Optional(Box::new(other))
        };
        result.optional = true;
    }
    result
}

#[derive(Clone, Debug)]
enum ResultWrapper {
    TryInto,
    Ok
}

impl ResultWrapper {
    fn wrap(&self, content: &str) -> String {
        match self {
            &ResultWrapper::TryInto => format!("{}.try_into().unwrap()", content),
            &ResultWrapper::Ok => format!("{}.try_into().ok()", content),
        }
    }
}

#[derive(Clone, Debug)]
struct ProcessedResult {
    type_: String,
    wrapper: ResultWrapper,
    optional: bool,
}

impl ProcessedResult {
    fn simple<S: Into<String>>(name: S) -> ProcessedResult {
        ProcessedResult { type_: name.into(), wrapper: ResultWrapper::TryInto, optional: false }
    }
}

fn process_result_type_kind(name: &str, type_kind: &TypeKind, registry: &Registry) -> ProcessedResult {
    match type_kind {
        &TypeKind::Primitive(ref p) => ProcessedResult::simple(p.name()),
        &TypeKind::String => ProcessedResult::simple("String"),
        &TypeKind::ArrayBuffer | &TypeKind::ArrayBufferView => ProcessedResult::simple("ArrayBuffer"),
        &TypeKind::BufferSource => unimplemented!("BufferSource not supported in output"),
        &TypeKind::CanvasElement => ProcessedResult::simple("CanvasElement"),
        &TypeKind::TypedArray(ref p) => ProcessedResult::simple(format!("TypedArray<{}>", p.name())),
        &TypeKind::Sequence(ref t) => {
            let inner = process_result_type(t, registry);
            ProcessedResult::simple(format!("Vec<{}>", inner.type_))
        },
        &TypeKind::Dictionary | &TypeKind::Interface | &TypeKind::Enum => ProcessedResult::simple(name),
        &TypeKind::Typedef(ref t) => {
            let inner = process_result_type(t, registry);
            ProcessedResult {
                type_: name.into(),
                wrapper: inner.wrapper.clone(),
                optional: inner.optional
            }
        },
        &TypeKind::Any | &TypeKind::Object => ProcessedResult::simple("Value")
    }
}

fn process_result_type(type_: &Type, registry: &Registry) -> ProcessedResult {
    let type_kind = registry.resolve_type(&type_.name);
    let mut result = process_result_type_kind(&type_.name, type_kind, registry);
    if type_.optional && !result.optional {
        result.type_ = format!("Option<{}>", result.type_);
        result.wrapper = ResultWrapper::Ok;
        result.optional = true;
    }
    result
}

fn write_header<W>(registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    writeln!(dest, r#"
// {registry:?}
extern crate stdweb;
extern crate serde;

use self::stdweb::{{Reference, Value, UnsafeTypedArray}};
use self::stdweb::private::*;
use self::stdweb::unstable::*;
use self::stdweb::web::{{RenderingContext, TypedArray, ArrayBuffer}};
use self::stdweb::web::html_element::CanvasElement;

type ConversionError = <Reference as TryFrom<Value>>::Error;

fn custom_error(s: &str) -> ConversionError {{
    use self::serde::ser::Error;
    stdweb::serde::ConversionError::custom(s).into()
}}

trait AsTypedArray<'a, T> {{
    type Result: JsSerializable;

    unsafe fn as_typed_array(self) -> Self::Result;
}}

macro_rules! define_array {{
    ($elem:ty) => {{
        impl<'a> AsTypedArray<'a, $elem> for &'a TypedArray<$elem> {{
            type Result = Self;

            unsafe fn as_typed_array(self) -> Self::Result {{ self }}
        }}

        impl<'a> AsTypedArray<'a, $elem> for &'a [$elem] {{
            type Result = UnsafeTypedArray<'a, $elem>;

            unsafe fn as_typed_array(self) -> Self::Result {{ UnsafeTypedArray::new(self) }}
        }}
    }}
}}

define_array!(i8);
define_array!(u8);
define_array!(i16);
define_array!(u16);
define_array!(i32);
define_array!(u32);
define_array!(f32);
define_array!(f64);
    "#, registry=registry)?;
    Ok(())
}

impl super::Generator for StdwebGenerator {
    fn write<W>(&self, registry: &Registry, dest: &mut W) -> io::Result<()>
        where W: io::Write
    {
        write_header(registry, dest)?;
        write_typedefs(registry, dest)?;
        write_enums(registry, dest)?;
        write_dictionaries(registry, dest)?;
        write_interfaces(registry, dest)?;
        Ok(())
    }
}

fn write_typedefs<W>(registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    for (name, type_kind) in &registry.types {
        match type_kind {
            &TypeKind::Typedef(ref t) => {
                write_typedef(name, t, registry, dest)?;
            },
            _ => {}
        }
    }
    Ok(())
}

fn write_typedef<W>(name: &str, type_: &Type, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    writeln!(dest, r#"pub type {name} = {type_};"#,
        name=name,
        type_=process_result_type(type_, registry).type_
    )?;
    Ok(())
}

fn write_enums<W>(registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    for (name, enum_) in &registry.enums {
        write_enum(name, enum_, registry, dest)?;
    }
    Ok(())
}

fn write_enum<W>(name: &str, enum_: &Enum, _registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    write!(dest, r#"
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum {name} {{
    "#, name=name)?;

    for variant in &enum_.variants {
        writeln!(dest, r#"
    #[serde(rename = "{raw_variant}")]
    {variant},"#,
        variant=camel(&variant),
        raw_variant=variant)?;
    }

    writeln!(dest, r#"
}}
js_deserializable!({name});
js_serializable!({name});
    "#, name=name)?;
    Ok(())
}

fn write_dictionaries<W>(registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    for (name, dictionary) in &registry.dictionaries {
        write_dictionary(name, dictionary, registry, dest)?;
    }
    Ok(())
}

fn write_dictionary<W>(name: &str, dictionary: &Dictionary, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    if dictionary.is_hidden {
        return Ok(());
    }

    write!(dest, r#"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {name} {{
    "#, name=name)?;

    for (name, field) in dictionary.collect_fields(registry) {
        write_field(name, field, registry, dest)?;
    }

    writeln!(dest, r#"
}}
js_deserializable!({name});
js_serializable!({name});
    "#, name=name)?;
    Ok(())
}

fn write_field<W>(name: &str, field: &Field, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    let mut serde_attrs = Vec::new();
    let field_name = unreserve(snake(name));
    if field_name != name {
        serde_attrs.push(format!(r#"rename = "{}""#, name));
    }
    let field_type = process_result_type(&field.type_, registry);
    if field_type.optional {
        serde_attrs.push(r#"default"#.into());
        serde_attrs.push(r#"skip_serializing_if = "Option::is_none""#.into());
    }

    if !serde_attrs.is_empty() {
        write!(dest, r#"
    #[serde({})]"#, serde_attrs.join(", "))?;
    }

    writeln!(dest, r#"
    {name}: {type_},"#, name=field_name, type_=field_type.type_)?;

    Ok(())
}

fn write_interfaces<W>(registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    for (name, interface) in &registry.interfaces {
        write_interface(name, interface, registry, dest)?;
    }
    Ok(())
}

fn write_interface<W>(name: &str, interface: &Interface, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    if interface.is_hidden {
        return Ok(());
    }

    write!(dest, r#"
#[derive(Debug, Clone)]
pub struct {name}(Reference);

impl FromReferenceUnchecked for {name} {{
    unsafe fn from_reference_unchecked(reference: Reference) -> Self {{
        {name}(reference)
    }}
}}

impl FromReference for {name} {{
    #[inline]
    fn from_reference(reference: Reference) -> Option<Self> {{
        if {{
            __js_raw_asm!(
                "return (Module.STDWEB.acquire_js_reference( $0 ) instanceof {name}) | 0;",
                reference.as_raw()
            ) == 1
        }} {{
            Some({name}(reference))
        }} else {{
            None
        }}
    }}
}}

impl AsRef<Reference> for {name} {{
    #[inline]
    fn as_ref(&self) -> &Reference {{
        &self.0
    }}
}}

impl From<{name}> for Reference {{
    #[inline]
    fn from(value: {name}) -> Self {{
        value.0
    }}
}}

impl TryFrom<{name}> for Reference {{
    type Error = Void;

    #[inline]
    fn try_from(value: {name}) -> Result<Self, Self::Error> {{
        Ok(value.0)
    }}
}}

impl<R: TryInto<Reference>> TryFrom<R> for {name}
    where <R as TryInto<Reference>>::Error: Into<ConversionError>
{{
    type Error = ConversionError;

    #[inline]
    fn try_from(value: R) -> Result<Self, Self::Error> {{
        value.try_into()
            .map_err(|error| error.into())
            .and_then(|reference: Reference| {{
                reference.downcast()
                    .ok_or_else(|| custom_error("reference is of a different type"))
            }})
    }}
}}

impl JsSerializable for {name} {{
    #[inline]
    fn into_js<'a>(&'a self, arena: &'a PreallocatedArena) -> SerializedValue<'a> {{
        self.0.into_js(arena)
    }}

    #[inline]
    fn memory_required(&self) -> usize {{
        Reference::memory_required(&self.0)
    }}
}}

__js_serializable_boilerplate!(() ({name}) ());

impl {name} {{
    "#, name=name)?;

    for (name, member) in interface.collect_members(registry, &VisitOptions::default()) {
        match member {
            &Member::Const(ref const_) => {
                write_const(name, const_, registry, dest)?;
            },
            &Member::Attribute(ref attribute) => {
                write_attribute(name, attribute, registry, dest)?;
            },
            &Member::Operation(ref operation) => {
                write_operation(name, operation, registry, dest)?;
            }
        }
    }

    writeln!(dest, r#"
}}
    "#)?;

    if let Some(rendering_context) = interface.rendering_context {
        writeln!(dest, r#"impl RenderingContext for {name} {{
    type Error = ConversionError;
    fn from_canvas(canvas: &CanvasElement) -> Result<Self, ConversionError> {{
        js!(
            return @{{canvas}}.getContext("{rendering_context}");
        ).try_into()
    }}
}}
        "#, name=name, rendering_context=rendering_context)?;
    }

    Ok(())
}

fn write_const<W>(name: &str, const_: &Const, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    let const_type = process_result_type(&const_.type_, registry);
    write!(dest, r#"
    pub const {name}: {type_} = {value};"#,
    name=shouty_snake(name),
    type_=const_type.type_,
    value=const_.value
    )?;
    Ok(())
}

fn write_attribute<W>(name: &str, attribute: &Attribute, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    if attribute.getter {
        let result_type = process_result_type(&attribute.type_, registry);
        let expr = result_type.wrapper.wrap(
            &format!("(js! {{ return @{{self}}.{raw_name}; }} )", raw_name=name)
        );

        write!(dest, r#"
    pub fn {name}(&self) -> {type_} {{
        {expr}
    }}
        "#,
        name=unreserve(snake(name)),
        type_=result_type.type_,
        expr=expr
        )?;
    }
    if attribute.setter {
        let mut gc = GenericContext::new();
        let arg_type = process_arg_type(&attribute.type_, registry, &mut gc);
        write!(dest, r#"
    pub fn set_{name}{gargs}(&self, value: {type_}){gwhere} {{
        js!( @{{self}}.{raw_name} = @{{{value}}}; );
    }}
        "#,
        name=snake(name),
        raw_name=name,
        type_=arg_type.type_,
        gargs=gc.args(),
        gwhere=gc.constraints(),
        value=arg_type.wrapper.wrap("value")
        )?;
    }
    Ok(())
}

fn write_operation<W>(name: &str, operation: &Operation, registry: &Registry, dest: &mut W) -> io::Result<()> where W: io::Write {
    let mut gc = GenericContext::new();

    struct OperationArg {
        arg: String,
        js_arg: String
    }

    let args: Vec<_> = operation.args.iter().map(|a| {
        let processed = process_arg_type(&a.type_, registry, &mut gc);
        let arg = format!("{}: {}", unreserve(snake(&a.name)), processed.type_);
        let js_arg = format!("@{{{}}}", processed.wrapper.wrap(&unreserve(snake(&a.name))));
        OperationArg { arg, js_arg }
    }).collect();

    let rust_args = args.iter().map(|a| a.arg.clone()).collect::<Vec<_>>().join(", ");
    let js_args = args.iter().map(|a| a.js_arg.clone()).collect::<Vec<_>>().join(", ");

    if let Some(return_type) = operation.return_type.as_ref() {
        let result_type = process_result_type(return_type, registry);
        let expr = result_type.wrapper.wrap(
            &format!("(js! {{ return @{{self}}.{raw_name}({js_args}); }} )", raw_name=name, js_args=js_args)
        );

        write!(dest, r#"

    pub fn {name}{gargs}(&self, {args}) -> {return_type}{gwhere} {{
        {expr}
    }}"#,
        name=unreserve(snake(name)),
        gargs=gc.args(),
        args=rust_args,
        return_type=result_type.type_,
        gwhere=gc.constraints(),
        expr=expr
        )?;
    } else {
        write!(dest, r#"

    pub fn {name}{gargs}(&self, {args}){gwhere} {{
        js!( @{{self}}.{raw_name}({js_args}); );
    }}"#,
        name=unreserve(snake(name)),
        raw_name=name,
        gargs=gc.args(),
        args=rust_args,
        gwhere=gc.constraints(),
        js_args=js_args
        )?;
    }
    Ok(())
}
