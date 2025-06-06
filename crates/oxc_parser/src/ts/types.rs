use oxc_allocator::{Box, Vec};
use oxc_ast::{NONE, ast::*};
use oxc_diagnostics::Result;
use oxc_span::GetSpan;
use oxc_syntax::operator::UnaryOperator;

use crate::{
    Context, ParserImpl, diagnostics,
    lexer::Kind,
    modifiers::{Modifier, ModifierFlags, ModifierKind, Modifiers},
};

impl<'a> ParserImpl<'a> {
    pub(crate) fn parse_ts_type(&mut self) -> Result<TSType<'a>> {
        if self.is_start_of_function_type_or_constructor_type() {
            return self.parse_function_or_constructor_type();
        }
        let span = self.start_span();
        let ty = self.parse_union_type_or_higher()?;
        if !self.ctx.has_disallow_conditional_types()
            && !self.cur_token().is_on_new_line
            && self.eat(Kind::Extends)
        {
            let extends_type = self.context(
                Context::DisallowConditionalTypes,
                Context::empty(),
                Self::parse_ts_type,
            )?;
            self.expect(Kind::Question)?;
            let true_type = self.context(
                Context::empty(),
                Context::DisallowConditionalTypes,
                Self::parse_ts_type,
            )?;
            self.expect(Kind::Colon)?;
            let false_type = self.context(
                Context::empty(),
                Context::DisallowConditionalTypes,
                Self::parse_ts_type,
            )?;
            return Ok(self.ast.ts_type_conditional_type(
                self.end_span(span),
                ty,
                extends_type,
                true_type,
                false_type,
            ));
        }
        Ok(ty)
    }

    fn parse_function_or_constructor_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        let r#abstract = self.eat(Kind::Abstract);
        let is_constructor_type = self.eat(Kind::New);
        let type_parameters = self.parse_ts_type_parameters()?;
        let (this_param, params) = self.parse_formal_parameters(FormalParameterKind::Signature)?;
        let return_type = {
            let return_type_span = self.start_span();
            let Some(return_type) =
                self.parse_return_type(Kind::Arrow, /* is_type */ false)?
            else {
                return Err(self.unexpected());
            };
            self.ast.ts_type_annotation(self.end_span(return_type_span), return_type)
        };

        let span = self.end_span(span);
        Ok(if is_constructor_type {
            if let Some(this_param) = &this_param {
                // type Foo = new (this: number) => any;
                self.error(diagnostics::ts_constructor_this_parameter(this_param.span));
            }
            self.ast.ts_type_constructor_type(
                span,
                r#abstract,
                type_parameters,
                params,
                return_type,
            )
        } else {
            self.ast.ts_type_function_type(span, type_parameters, this_param, params, return_type)
        })
    }

    fn is_start_of_function_type_or_constructor_type(&mut self) -> bool {
        if self.at(Kind::LAngle) {
            return true;
        }
        if self.at(Kind::LParen) && self.lookahead(Self::is_unambiguously_start_of_function_type) {
            return true;
        }
        self.at(Kind::New) || (self.at(Kind::Abstract) && self.peek_at(Kind::New))
    }

    fn is_unambiguously_start_of_function_type(&mut self) -> bool {
        self.bump_any();
        // ( )
        // ( ...
        if matches!(self.cur_kind(), Kind::RParen | Kind::Dot3) {
            return true;
        }
        if self.skip_parameter_start() {
            // ( xxx :
            // ( xxx ,
            // ( xxx ?
            // ( xxx =
            if matches!(self.cur_kind(), Kind::Colon | Kind::Comma | Kind::Question | Kind::Eq) {
                return true;
            }
            // ( xxx ) =>
            if self.eat(Kind::RParen) && self.at(Kind::Arrow) {
                return true;
            }
        }
        false
    }

    fn skip_parameter_start(&mut self) -> bool {
        // Skip modifiers
        if self.cur_kind().is_modifier_kind() {
            self.parse_modifiers(false, false, false);
        }
        if self.cur_kind().is_identifier() || self.at(Kind::This) {
            self.bump_any();
            return true;
        }
        if matches!(self.cur_kind(), Kind::LBrack | Kind::LCurly) {
            let errors_count = self.errors_count();
            if self.parse_binding_pattern_kind().is_ok() && errors_count == self.errors_count() {
                return true;
            }
        }
        false
    }

    pub(crate) fn parse_ts_type_parameters(
        &mut self,
    ) -> Result<Option<Box<'a, TSTypeParameterDeclaration<'a>>>> {
        if !self.is_ts {
            return Ok(None);
        }
        if !self.at(Kind::LAngle) {
            return Ok(None);
        }
        let span = self.start_span();
        self.expect(Kind::LAngle)?;
        let params = self.parse_delimited_list(
            Kind::RAngle,
            Kind::Comma,
            /* trailing_separator */ true,
            Self::parse_ts_type_parameter,
        )?;
        self.expect(Kind::RAngle)?;
        Ok(Some(self.ast.alloc_ts_type_parameter_declaration(self.end_span(span), params)))
    }

    pub(crate) fn parse_ts_implements_clause(&mut self) -> Result<Vec<'a, TSClassImplements<'a>>> {
        self.expect(Kind::Implements)?;
        let first = self.parse_ts_implement_name()?;
        let mut implements = self.ast.vec();
        implements.push(first);

        while self.eat(Kind::Comma) {
            implements.push(self.parse_ts_implement_name()?);
        }

        Ok(implements)
    }

    pub(crate) fn parse_ts_type_parameter(&mut self) -> Result<TSTypeParameter<'a>> {
        let span = self.start_span();

        let modifiers = self.parse_modifiers(false, true, false);
        self.verify_modifiers(
            &modifiers,
            ModifierFlags::IN | ModifierFlags::OUT | ModifierFlags::CONST,
            diagnostics::cannot_appear_on_a_type_parameter,
        );

        let name = self.parse_binding_identifier()?;
        let constraint = self.parse_ts_type_constraint()?;
        let default = self.parse_ts_default_type()?;

        Ok(self.ast.ts_type_parameter(
            self.end_span(span),
            name,
            constraint,
            default,
            modifiers.contains(ModifierKind::In),
            modifiers.contains(ModifierKind::Out),
            modifiers.contains(ModifierKind::Const),
        ))
    }

    fn parse_intersection_type_or_higher(&mut self) -> Result<TSType<'a>> {
        self.parse_union_type_or_intersection_type(Kind::Amp, Self::parse_type_operator_or_higher)
    }

    fn parse_union_type_or_higher(&mut self) -> Result<TSType<'a>> {
        self.parse_union_type_or_intersection_type(
            Kind::Pipe,
            Self::parse_intersection_type_or_higher,
        )
    }

    fn parse_union_type_or_intersection_type<F>(
        &mut self,
        kind: Kind,
        parse_constituent_type: F,
    ) -> Result<TSType<'a>>
    where
        F: Fn(&mut Self) -> Result<TSType<'a>>,
    {
        let span = self.start_span();
        let has_leading_operator = self.eat(kind);
        /* hasLeadingOperator && parseFunctionOrConstructorTypeToError(isUnionType) ||*/
        let mut ty = parse_constituent_type(self)?;
        if self.at(kind) || has_leading_operator {
            let mut types = self.ast.vec1(ty);
            while self.eat(kind) {
                types.push(
                    /*parseFunctionOrConstructorTypeToError(isUnionType) || */
                    parse_constituent_type(self)?,
                );
            }
            let span = self.end_span(span);
            ty = match kind {
                Kind::Pipe => self.ast.ts_type_union_type(span, types),
                Kind::Amp => self.ast.ts_type_intersection_type(span, types),
                _ => unreachable!(),
            };
        }
        Ok(ty)
    }

    fn parse_type_operator_or_higher(&mut self) -> Result<TSType<'a>> {
        match self.cur_kind() {
            Kind::KeyOf => self.parse_type_operator(TSTypeOperatorOperator::Keyof),
            Kind::Unique => self.parse_type_operator(TSTypeOperatorOperator::Unique),
            Kind::Readonly => self.parse_type_operator(TSTypeOperatorOperator::Readonly),
            Kind::Infer => self.parse_infer_type(),
            _ => self.context(
                Context::empty(),
                Context::DisallowConditionalTypes,
                Self::parse_postfix_type_or_higher,
            ),
        }
    }

    fn parse_type_operator(&mut self, operator: TSTypeOperatorOperator) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // bump operator
        let operator_span = self.end_span(span);
        let ty = self.parse_type_operator_or_higher()?;
        if operator == TSTypeOperatorOperator::Readonly
            && !matches!(ty, TSType::TSArrayType(_))
            && !matches!(ty, TSType::TSTupleType(_))
        {
            self.error(diagnostics::readonly_in_array_or_tuple_type(operator_span));
        }
        Ok(self.ast.ts_type_type_operator_type(self.end_span(span), operator, ty))
    }

    fn parse_infer_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // bump `infer`
        let type_parameter = self.parse_type_parameter_of_infer_type()?;
        Ok(self.ast.ts_type_infer_type(self.end_span(span), type_parameter))
    }

    fn parse_type_parameter_of_infer_type(&mut self) -> Result<Box<'a, TSTypeParameter<'a>>> {
        let span = self.start_span();
        let name = self.parse_binding_identifier()?;
        let constraint = self.try_parse(Self::try_parse_constraint_of_infer_type).unwrap_or(None);
        let span = self.end_span(span);
        let ts_type_parameter =
            self.ast.alloc_ts_type_parameter(span, name, constraint, None, false, false, false);
        Ok(ts_type_parameter)
    }

    fn parse_postfix_type_or_higher(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        let mut ty = self.parse_non_array_type()?;

        while !self.cur_token().is_on_new_line {
            match self.cur_kind() {
                Kind::Bang => {
                    self.bump_any();
                    ty = self.ast.ts_type_js_doc_non_nullable_type(
                        self.end_span(span),
                        ty,
                        /* postfix */ true,
                    );
                }
                Kind::Question => {
                    // If next token is start of a type we have a conditional type
                    if self.lookahead(Self::next_token_is_start_of_type) {
                        return Ok(ty);
                    }
                    self.bump_any();
                    ty = self.ast.ts_type_js_doc_nullable_type(
                        self.end_span(span),
                        ty,
                        /* postfix */ true,
                    );
                }
                Kind::LBrack => {
                    self.bump_any();
                    if self.is_start_of_type(/* in_start_of_parameter */ false) {
                        let index_type = self.parse_ts_type()?;
                        self.expect(Kind::RBrack)?;
                        ty = self.ast.ts_type_indexed_access_type(
                            self.end_span(span),
                            ty,
                            index_type,
                        );
                    } else {
                        self.expect(Kind::RBrack)?;
                        ty = self.ast.ts_type_array_type(self.end_span(span), ty);
                    }
                }
                _ => return Ok(ty),
            }
        }
        Ok(ty)
    }

    fn parse_non_array_type(&mut self) -> Result<TSType<'a>> {
        match self.cur_kind() {
            Kind::Any
            | Kind::Unknown
            | Kind::String
            | Kind::Number
            | Kind::BigInt
            | Kind::Symbol
            | Kind::Boolean
            | Kind::Undefined
            | Kind::Never
            | Kind::Object
            // Parse `null` as `TSNullKeyword` instead of null literal to align with typescript eslint.
            | Kind::Null => {
                if let Some(ty) = self.try_parse(Self::parse_keyword_and_no_dot) {
                    Ok(ty)
                } else {
                    self.parse_type_reference()
                }
            }
            // TODO: js doc types: `JSDocAllType`, `JSDocFunctionType`
            // Kind::StarEq => {
            // scanner.reScanAsteriskEqualsToken();
            // falls through
            // }
            // Kind::Star => {
            // return parseJSDocAllType();
            // }
            // case SyntaxKind.QuestionQuestionToken:
            // // If there is '??', treat it as prefix-'?' in JSDoc type.
            // scanner.reScanQuestionToken();
            // // falls through
            // case SyntaxKind.FunctionKeyword:
            // return parseJSDocFunctionType();
            Kind::Question => self.parse_js_doc_unknown_or_nullable_type(),
            Kind::Bang => self.parse_js_doc_non_nullable_type(),
            Kind::NoSubstitutionTemplate | Kind::Str | Kind::True | Kind::False => {
                self.parse_literal_type_node(/* negative */ false)
            }
            kind if kind.is_number() => {
                self.parse_literal_type_node(/* negative */ false)
            }
            Kind::Minus => {
                if self.peek_kind().is_number() {
                    self.parse_literal_type_node(/* negative */ true)
                } else {
                    self.parse_type_reference()
                }
            }
            Kind::Void => {
                let span = self.start_span();
                self.bump_any();
                Ok(self.ast.ts_type_void_keyword(self.end_span(span)))
            }
            Kind::This => {
                let span = self.start_span();
                self.bump_any(); // bump `this`
                let this_type = self.ast.ts_this_type(self.end_span(span));
                if self.peek_at(Kind::Is) && !self.peek_token().is_on_new_line {
                    return self.parse_this_type_predicate(this_type);
                }
                Ok(TSType::TSThisType(self.alloc(this_type)))
            }
            Kind::Typeof => {
                self.parse_type_query()
            }
            Kind::LCurly => {
                if self.lookahead(Self::is_start_of_mapped_type) {
                    self.parse_mapped_type()
                } else {
                    self.parse_type_literal()
                }
            }
            Kind::LBrack => self.parse_tuple_type(),
            Kind::LParen => self.parse_parenthesized_type(),
            Kind::Import => self.parse_ts_import_type().map(
                TSType::TSImportType
            ),
            Kind::Asserts => {
                let peek_token = self.peek_token();
                if peek_token.kind.is_identifier_name() && !peek_token.is_on_new_line {
                    self.parse_asserts_type_predicate()
                } else {
                    self.parse_type_reference()
                }
            }
            Kind::TemplateHead => self.parse_template_type(false),
            _ => self.parse_type_reference(),
        }
    }

    fn parse_keyword_and_no_dot(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        let ty = match self.cur_kind() {
            Kind::Any => {
                self.bump_any();
                self.ast.ts_type_any_keyword(self.end_span(span))
            }
            Kind::BigInt => {
                self.bump_any();
                self.ast.ts_type_big_int_keyword(self.end_span(span))
            }
            Kind::Boolean => {
                self.bump_any();
                self.ast.ts_type_boolean_keyword(self.end_span(span))
            }
            Kind::Never => {
                self.bump_any();
                self.ast.ts_type_never_keyword(self.end_span(span))
            }
            Kind::Number => {
                self.bump_any();
                self.ast.ts_type_number_keyword(self.end_span(span))
            }
            Kind::Object => {
                self.bump_any();
                self.ast.ts_type_object_keyword(self.end_span(span))
            }
            Kind::String => {
                self.bump_any();
                self.ast.ts_type_string_keyword(self.end_span(span))
            }
            Kind::Symbol => {
                self.bump_any();
                self.ast.ts_type_symbol_keyword(self.end_span(span))
            }
            Kind::Undefined => {
                self.bump_any();
                self.ast.ts_type_undefined_keyword(self.end_span(span))
            }
            Kind::Unknown => {
                self.bump_any();
                self.ast.ts_type_unknown_keyword(self.end_span(span))
            }
            Kind::Null => {
                self.bump_any();
                self.ast.ts_type_null_keyword(self.end_span(span))
            }
            _ => return Err(self.unexpected()),
        };
        if self.at(Kind::Dot) {
            return Err(self.unexpected());
        }
        Ok(ty)
    }

    fn is_start_of_type(&mut self, in_start_of_parameter: bool) -> bool {
        match self.cur_kind() {
            kind if kind.is_number() => true,
            Kind::Any
            | Kind::Unknown
            | Kind::String
            | Kind::Number
            | Kind::BigInt
            | Kind::Boolean
            | Kind::Readonly
            | Kind::Symbol
            | Kind::Unique
            | Kind::Void
            | Kind::Undefined
            | Kind::Null
            | Kind::This
            | Kind::Typeof
            | Kind::Never
            | Kind::LCurly
            | Kind::LBrack
            | Kind::LAngle
            | Kind::Pipe
            | Kind::Amp
            | Kind::New
            | Kind::Str
            | Kind::True
            | Kind::False
            | Kind::Object
            | Kind::Star
            | Kind::Question
            | Kind::Break
            | Kind::Dot3
            | Kind::Infer
            | Kind::Import
            | Kind::Asserts
            | Kind::NoSubstitutionTemplate
            | Kind::TemplateHead => true,
            Kind::Function => !in_start_of_parameter,
            Kind::Minus => !in_start_of_parameter && self.peek_kind().is_number(),
            Kind::LParen => {
                !in_start_of_parameter
                    && self.lookahead(Self::is_start_of_parenthesized_or_function_type)
            }
            kind => kind.is_identifier(),
        }
    }

    fn is_start_of_mapped_type(&mut self) -> bool {
        if !self.at(Kind::LCurly) {
            return false;
        }

        if self.peek_at(Kind::Plus) || self.peek_at(Kind::Minus) {
            return self.nth_at(2, Kind::Readonly);
        }

        let mut offset = 1;

        if self.nth_at(offset, Kind::Readonly) {
            offset += 1;
        }

        self.nth_at(offset, Kind::LBrack)
            && self.nth_kind(offset + 1).is_identifier_name()
            && self.nth_at(offset + 2, Kind::In)
    }

    fn next_token_is_start_of_type(&mut self) -> bool {
        self.bump_any();
        self.is_start_of_type(false)
    }

    fn is_start_of_parenthesized_or_function_type(&mut self) -> bool {
        self.bump_any();
        self.at(Kind::RParen)
            || self.is_start_of_parameter(/* is_js_doc_parameter */ false)
            || self.is_start_of_type(false)
    }

    fn is_start_of_parameter(&mut self, is_js_doc_parameter: bool) -> bool {
        let kind = self.cur_kind();
        kind == Kind::Dot3
            || kind.is_binding_identifier_or_private_identifier_or_pattern()
            || kind.is_modifier_kind()
            || kind == Kind::At
            || self.is_start_of_type(!is_js_doc_parameter)
    }

    fn parse_mapped_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.expect(Kind::LCurly)?;
        let mut readonly = TSMappedTypeModifierOperator::None;
        if self.eat(Kind::Readonly) {
            readonly = TSMappedTypeModifierOperator::True;
        } else if self.eat(Kind::Plus) && self.eat(Kind::Readonly) {
            readonly = TSMappedTypeModifierOperator::Plus;
        } else if self.eat(Kind::Minus) && self.eat(Kind::Readonly) {
            readonly = TSMappedTypeModifierOperator::Minus;
        }

        self.expect(Kind::LBrack)?;
        let type_parameter_span = self.start_span();
        if !self.cur_kind().is_identifier_name() {
            return Err(self.unexpected());
        }
        let name = self.parse_binding_identifier()?;
        self.expect(Kind::In)?;
        let constraint = self.parse_ts_type()?;
        let type_parameter = self.alloc(self.ast.ts_type_parameter(
            self.end_span(type_parameter_span),
            name,
            Some(constraint),
            None,
            false,
            false,
            false,
        ));

        let name_type = if self.eat(Kind::As) { Some(self.parse_ts_type()?) } else { None };
        self.expect(Kind::RBrack)?;

        let optional = match self.cur_kind() {
            Kind::Question => {
                self.bump_any();
                TSMappedTypeModifierOperator::True
            }
            Kind::Minus => {
                self.bump_any();
                self.expect(Kind::Question)?;
                TSMappedTypeModifierOperator::Minus
            }
            Kind::Plus => {
                self.bump_any();
                self.expect(Kind::Question)?;
                TSMappedTypeModifierOperator::Plus
            }
            _ => TSMappedTypeModifierOperator::None,
        };

        let type_annotation = self.eat(Kind::Colon).then(|| self.parse_ts_type()).transpose()?;
        self.bump(Kind::Semicolon);
        self.expect(Kind::RCurly)?;

        Ok(self.ast.ts_type_mapped_type(
            self.end_span(span),
            type_parameter,
            name_type,
            type_annotation,
            optional,
            readonly,
        ))
    }

    fn parse_type_literal(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        let member_list =
            self.parse_normal_list(Kind::LCurly, Kind::RCurly, Self::parse_ts_type_signature)?;
        Ok(self.ast.ts_type_type_literal(self.end_span(span), member_list))
    }

    fn parse_type_query(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // `bump `typeof`
        let (entity_name, type_arguments) = if self.at(Kind::Import) {
            let entity_name = TSTypeQueryExprName::TSImportType(self.parse_ts_import_type()?);
            (entity_name, None)
        } else {
            let entity_name = self.parse_ts_type_name()?; // TODO: parseEntityName
            let entity_name = TSTypeQueryExprName::from(entity_name);
            let type_arguments = if self.cur_token().is_on_new_line {
                None
            } else {
                self.try_parse_type_arguments()?
            };
            (entity_name, type_arguments)
        };
        Ok(self.ast.ts_type_type_query(self.end_span(span), entity_name, type_arguments))
    }

    fn parse_this_type_predicate(&mut self, this_ty: TSThisType) -> Result<TSType<'a>> {
        let span = this_ty.span;
        self.bump_any(); // bump `is`
        // TODO: this should go through the ast builder.
        let parameter_name = TSTypePredicateName::This(this_ty);
        let type_span = self.start_span();
        let ty = self.parse_ts_type()?;
        let type_annotation = Some(self.ast.ts_type_annotation(self.end_span(type_span), ty));
        Ok(self.ast.ts_type_type_predicate(
            self.end_span(span),
            parameter_name,
            false,
            type_annotation,
        ))
    }

    fn parse_this_type_node(&mut self) -> TSThisType {
        let span = self.start_span();
        self.bump_any(); // bump `this`
        self.ast.ts_this_type(self.end_span(span))
    }

    fn parse_ts_type_constraint(&mut self) -> Result<Option<TSType<'a>>> {
        if !self.at(Kind::Extends) {
            return Ok(None);
        }
        self.bump_any();
        Ok(Some(self.parse_ts_type()?))
    }

    fn parse_ts_default_type(&mut self) -> Result<Option<TSType<'a>>> {
        if !self.at(Kind::Eq) {
            return Ok(None);
        }
        self.bump_any();
        Ok(Some(self.parse_ts_type()?))
    }

    fn parse_template_type(&mut self, tagged: bool) -> Result<TSType<'a>> {
        let span = self.start_span();
        let mut types = self.ast.vec();
        let mut quasis = self.ast.vec();
        match self.cur_kind() {
            Kind::NoSubstitutionTemplate => {
                quasis.push(self.parse_template_element(tagged));
            }
            Kind::TemplateHead => {
                quasis.push(self.parse_template_element(tagged));
                types.push(self.parse_ts_type()?);
                self.re_lex_template_substitution_tail();
                loop {
                    match self.cur_kind() {
                        Kind::Eof => self.expect(Kind::TemplateTail)?,
                        Kind::TemplateTail => {
                            quasis.push(self.parse_template_element(tagged));
                            break;
                        }
                        Kind::TemplateMiddle => {
                            quasis.push(self.parse_template_element(tagged));
                        }
                        _ => {
                            types.push(self.parse_ts_type()?);
                            self.re_lex_template_substitution_tail();
                        }
                    }
                }
            }
            _ => unreachable!("parse_template_literal"),
        }

        Ok(self.ast.ts_type_template_literal_type(self.end_span(span), quasis, types))
    }

    fn parse_asserts_type_predicate(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // bump `asserts`
        let parameter_name = if self.at(Kind::This) {
            TSTypePredicateName::This(self.parse_this_type_node())
        } else {
            let ident_name = self.parse_identifier_name()?;
            TSTypePredicateName::Identifier(self.alloc(ident_name))
        };
        let mut type_annotation = None;
        if self.eat(Kind::Is) {
            let type_span = self.start_span();
            let ty = self.parse_ts_type()?;
            type_annotation = Some(self.ast.ts_type_annotation(self.end_span(type_span), ty));
        }
        Ok(self.ast.ts_type_type_predicate(
            self.end_span(span),
            parameter_name,
            /* asserts */ true,
            type_annotation,
        ))
    }

    fn parse_type_reference(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        let type_name = self.parse_ts_type_name()?;
        let type_parameters = self.parse_type_arguments_of_type_reference()?;
        Ok(self.ast.ts_type_type_reference(self.end_span(span), type_name, type_parameters))
    }

    fn parse_ts_implement_name(&mut self) -> Result<TSClassImplements<'a>> {
        let span = self.start_span();
        let type_name = self.parse_ts_type_name()?;
        let type_parameters = self.parse_type_arguments_of_type_reference()?;
        Ok(self.ast.ts_class_implements(self.end_span(span), type_name, type_parameters))
    }

    pub(crate) fn parse_ts_type_name(&mut self) -> Result<TSTypeName<'a>> {
        let span = self.start_span();
        let ident = self.parse_identifier_name()?;
        let ident = self.ast.alloc_identifier_reference(ident.span, ident.name);
        let mut left = TSTypeName::IdentifierReference(ident);
        while self.eat(Kind::Dot) {
            let right = self.parse_identifier_name()?;
            left = self.ast.ts_type_name_qualified_name(self.end_span(span), left, right);
        }
        Ok(left)
    }

    pub(crate) fn try_parse_type_arguments(
        &mut self,
    ) -> Result<Option<Box<'a, TSTypeParameterInstantiation<'a>>>> {
        if self.at(Kind::LAngle) {
            let span = self.start_span();
            self.expect(Kind::LAngle)?;
            let params = self.parse_delimited_list(
                Kind::RAngle,
                Kind::Comma,
                /* trailing_separator */ true,
                Self::parse_ts_type,
            )?;
            self.expect(Kind::RAngle)?;
            return Ok(Some(
                self.ast.alloc_ts_type_parameter_instantiation(self.end_span(span), params),
            ));
        }
        Ok(None)
    }

    fn parse_type_arguments_of_type_reference(
        &mut self,
    ) -> Result<Option<Box<'a, TSTypeParameterInstantiation<'a>>>> {
        self.re_lex_l_angle();
        if !self.cur_token().is_on_new_line && self.re_lex_l_angle() == Kind::LAngle {
            let span = self.start_span();
            self.expect(Kind::LAngle)?;
            let params = self.parse_delimited_list(
                Kind::RAngle,
                Kind::Comma,
                /* trailing_separator */ true,
                Self::parse_ts_type,
            )?;
            self.expect(Kind::RAngle)?;
            return Ok(Some(
                self.ast.alloc_ts_type_parameter_instantiation(self.end_span(span), params),
            ));
        }
        Ok(None)
    }

    pub(crate) fn parse_type_arguments_in_expression(
        &mut self,
    ) -> Result<Option<Box<'a, TSTypeParameterInstantiation<'a>>>> {
        if !self.is_ts {
            return Ok(None);
        }
        let span = self.start_span();
        if self.re_lex_l_angle() != Kind::LAngle {
            return Ok(None);
        }
        self.expect(Kind::LAngle)?;
        let params = self.parse_delimited_list(
            Kind::RAngle,
            Kind::Comma,
            /* trailing_separator */ true,
            Self::parse_ts_type,
        )?;
        // `a < b> = c`` is valid but `a < b >= c` is BinaryExpression
        if matches!(self.re_lex_right_angle(), Kind::GtEq) {
            return Err(self.unexpected());
        }
        self.re_lex_ts_r_angle();
        self.expect(Kind::RAngle)?;
        if self.can_follow_type_arguments_in_expr() {
            return Ok(Some(
                self.ast.alloc_ts_type_parameter_instantiation(self.end_span(span), params),
            ));
        }
        Err(self.unexpected())
    }

    fn can_follow_type_arguments_in_expr(&mut self) -> bool {
        match self.cur_kind() {
            Kind::LParen | Kind::NoSubstitutionTemplate | Kind::TemplateHead => true,
            Kind::LAngle | Kind::RAngle | Kind::Plus | Kind::Minus => false,
            _ => {
                self.cur_token().is_on_new_line
                    || self.is_binary_operator()
                    || !self.is_start_of_expression()
            }
        }
    }

    fn parse_tuple_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.expect(Kind::LBrack)?;
        let elements = self.parse_delimited_list(
            Kind::RBrack,
            Kind::Comma,
            /* trailing_separator */ true,
            Self::parse_tuple_element_name_or_tuple_element_type,
        )?;
        self.expect(Kind::RBrack)?;
        Ok(self.ast.ts_type_tuple_type(self.end_span(span), elements))
    }

    pub(super) fn parse_tuple_element_name_or_tuple_element_type(
        &mut self,
    ) -> Result<TSTupleElement<'a>> {
        if self.lookahead(Self::is_tuple_element_name) {
            let span = self.start_span();
            let dotdotdot = self.eat(Kind::Dot3);
            let member_span = self.start_span();
            let label = self.parse_identifier_name()?;
            let optional = self.eat(Kind::Question);
            self.expect(Kind::Colon)?;
            let element_type = self.parse_tuple_element_type()?;
            let span = self.end_span(span);
            return Ok(if dotdotdot {
                let type_annotation = self.ast.ts_type_named_tuple_member(
                    self.end_span(member_span),
                    element_type,
                    label,
                    // TODO: A tuple member cannot be both optional and rest. (TS5085)
                    // See typescript suite <conformance/types/tuple/restTupleElements1.ts>
                    optional,
                );
                self.ast.ts_tuple_element_rest_type(span, type_annotation)
            } else {
                TSTupleElement::from(self.ast.ts_type_named_tuple_member(
                    span,
                    element_type,
                    label,
                    optional,
                ))
            });
        }
        self.parse_tuple_element_type()
    }

    fn is_tuple_element_name(&mut self) -> bool {
        if self.eat(Kind::Dot3) {
            return self.cur_kind().is_identifier_name()
                && self.is_next_token_colon_or_question_colon();
        }
        self.cur_kind().is_identifier_name() && self.is_next_token_colon_or_question_colon()
    }

    fn is_next_token_colon_or_question_colon(&mut self) -> bool {
        self.bump_any();
        if self.at(Kind::Colon) {
            return true;
        }
        self.at(Kind::Question) && self.peek_at(Kind::Colon)
    }

    fn parse_tuple_element_type(&mut self) -> Result<TSTupleElement<'a>> {
        let span = self.start_span();
        if self.eat(Kind::Dot3) {
            let ty = self.parse_ts_type()?;
            return Ok(self.ast.ts_tuple_element_rest_type(self.end_span(span), ty));
        }
        let ty = self.parse_ts_type()?;
        if let TSType::JSDocNullableType(ty) = ty {
            if ty.span.start == ty.type_annotation.span().start {
                Ok(self.ast.ts_tuple_element_optional_type(ty.span, ty.unbox().type_annotation))
            } else {
                Ok(TSTupleElement::JSDocNullableType(ty))
            }
        } else {
            Ok(TSTupleElement::from(ty))
        }
    }

    fn parse_parenthesized_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // bump `(`
        let ty = self.parse_ts_type()?;
        self.expect(Kind::RParen)?;
        Ok(if self.options.preserve_parens {
            self.ast.ts_type_parenthesized_type(self.end_span(span), ty)
        } else {
            ty
        })
    }

    fn parse_literal_type_node(&mut self, negative: bool) -> Result<TSType<'a>> {
        let span = self.start_span();
        if negative {
            self.bump_any(); // bump `-`
        }

        let expression = if self.at(Kind::NoSubstitutionTemplate) {
            self.parse_template_literal_expression(false)
        } else {
            self.parse_literal_expression()
        }?;

        let span = self.end_span(span);
        let literal = if negative {
            match self.ast.expression_unary(span, UnaryOperator::UnaryNegation, expression) {
                Expression::UnaryExpression(unary_expr) => TSLiteral::UnaryExpression(unary_expr),
                _ => unreachable!(),
            }
        } else {
            match expression {
                Expression::BooleanLiteral(literal) => TSLiteral::BooleanLiteral(literal),
                Expression::NumericLiteral(literal) => TSLiteral::NumericLiteral(literal),
                Expression::BigIntLiteral(literal) => TSLiteral::BigIntLiteral(literal),
                Expression::StringLiteral(literal) => TSLiteral::StringLiteral(literal),
                Expression::TemplateLiteral(literal) => TSLiteral::TemplateLiteral(literal),
                _ => return Err(self.unexpected()),
            }
        };

        Ok(self.ast.ts_type_literal_type(span, literal))
    }

    fn parse_ts_import_type(&mut self) -> Result<Box<'a, TSImportType<'a>>> {
        let span = self.start_span();
        self.expect(Kind::Import)?;
        self.expect(Kind::LParen)?;
        let argument = self.parse_ts_type()?;
        let options =
            if self.eat(Kind::Comma) { Some(self.parse_object_expression()?) } else { None };
        self.expect(Kind::RParen)?;
        let qualifier = if self.eat(Kind::Dot) { Some(self.parse_ts_type_name()?) } else { None };
        let type_arguments = self.parse_type_arguments_of_type_reference()?;
        Ok(self.ast.alloc_ts_import_type(
            self.end_span(span),
            argument,
            options,
            qualifier,
            type_arguments,
        ))
    }

    fn try_parse_constraint_of_infer_type(&mut self) -> Result<Option<TSType<'a>>> {
        if self.eat(Kind::Extends) {
            let constraint = self.context(
                Context::DisallowConditionalTypes,
                Context::empty(),
                Self::parse_ts_type,
            )?;
            if self.ctx.has_disallow_conditional_types() || !self.at(Kind::Question) {
                return Ok(Some(constraint));
            }
        }
        Err(self.unexpected())
    }

    pub(crate) fn parse_ts_return_type_annotation(
        &mut self,
        kind: Kind,
        is_type: bool,
    ) -> Result<Option<Box<'a, TSTypeAnnotation<'a>>>> {
        if !self.is_ts {
            return Ok(None);
        }
        if !self.at(Kind::Colon) {
            return Ok(None);
        }
        let span = self.start_span();
        Ok(self
            .parse_return_type(kind, is_type)?
            .map(|return_type| self.ast.alloc_ts_type_annotation(self.end_span(span), return_type)))
    }

    fn parse_return_type(
        &mut self,
        return_kind: Kind,
        is_type: bool,
    ) -> Result<Option<TSType<'a>>> {
        if self.should_parse_return_type(return_kind, is_type) {
            return self
                .context(
                    Context::empty(),
                    Context::DisallowConditionalTypes,
                    Self::parse_type_or_type_predicate,
                )
                .map(Some);
        }
        Ok(None)
    }

    fn should_parse_return_type(&mut self, return_kind: Kind, _is_type: bool) -> bool {
        if return_kind == Kind::Arrow {
            self.bump_any();
            return true;
        }
        if self.eat(Kind::Colon) {
            return true;
        }
        // TODO
        // if (isType && token() === SyntaxKind.EqualsGreaterThanToken) {
        // // This is easy to get backward, especially in type contexts, so parse the type anyway
        // parseErrorAtCurrentToken(Diagnostics._0_expected, tokenToString(SyntaxKind.ColonToken));
        // nextToken();
        // return true;
        // }
        false
    }

    fn parse_type_or_type_predicate(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        let type_predicate_variable = self.try_parse(Self::parse_type_predicate_prefix);
        let type_span = self.start_span();
        let ty = self.parse_ts_type()?;
        if let Some(parameter_name) = type_predicate_variable {
            let type_annotation = Some(self.ast.ts_type_annotation(self.end_span(type_span), ty));
            return Ok(self.ast.ts_type_type_predicate(
                self.end_span(span),
                parameter_name,
                false,
                type_annotation,
            ));
        }
        Ok(ty)
    }

    fn parse_type_predicate_prefix(&mut self) -> Result<TSTypePredicateName<'a>> {
        let parameter_name = if self.at(Kind::This) {
            TSTypePredicateName::This(self.parse_this_type_node())
        } else {
            let ident_name = self.parse_identifier_name()?;
            TSTypePredicateName::Identifier(self.alloc(ident_name))
        };
        let token = self.cur_token();
        if token.kind == Kind::Is && !token.is_on_new_line {
            self.bump_any();
            return Ok(parameter_name);
        }
        Err(self.unexpected())
    }

    pub(crate) fn is_next_at_type_member_name(&mut self) -> bool {
        self.peek_kind().is_literal_property_name() || self.peek_at(Kind::LBrack)
    }

    pub(crate) fn parse_ts_call_signature_member(&mut self) -> Result<TSSignature<'a>> {
        let span = self.start_span();
        let type_parameters = self.parse_ts_type_parameters()?;
        let (this_param, params) = self.parse_formal_parameters(FormalParameterKind::Signature)?;
        let return_type = self.parse_ts_return_type_annotation(Kind::Colon, false)?;
        self.parse_type_member_semicolon();
        Ok(self.ast.ts_signature_call_signature_declaration(
            self.end_span(span),
            type_parameters,
            this_param,
            params,
            return_type,
        ))
    }

    pub(crate) fn parse_ts_getter_signature_member(&mut self) -> Result<TSSignature<'a>> {
        let span = self.start_span();
        self.expect(Kind::Get)?;
        let (key, computed) = self.parse_property_name()?;
        let (this_param, params) = self.parse_formal_parameters(FormalParameterKind::Signature)?;
        let return_type = self.parse_ts_return_type_annotation(Kind::Colon, false)?;
        self.parse_type_member_semicolon();
        Ok(self.ast.ts_signature_method_signature(
            self.end_span(span),
            key,
            computed,
            /* optional */ false,
            TSMethodSignatureKind::Get,
            NONE,
            this_param,
            params,
            return_type,
        ))
    }

    pub(crate) fn parse_ts_setter_signature_member(&mut self) -> Result<TSSignature<'a>> {
        let span = self.start_span();
        self.expect(Kind::Set)?;
        let (key, computed) = self.parse_property_name()?;
        let (this_param, params) = self.parse_formal_parameters(FormalParameterKind::Signature)?;
        let return_type = self.parse_ts_return_type_annotation(Kind::Colon, false)?;
        self.parse_type_member_semicolon();
        if let Some(return_type) = return_type.as_ref() {
            self.error(diagnostics::a_set_accessor_cannot_have_a_return_type_annotation(
                return_type.span,
            ));
        }
        Ok(self.ast.ts_signature_method_signature(
            self.end_span(span),
            key,
            computed,
            /* optional */ false,
            TSMethodSignatureKind::Set,
            NONE,
            this_param,
            params,
            return_type,
        ))
    }

    pub(crate) fn parse_ts_property_or_method_signature_member(
        &mut self,
    ) -> Result<TSSignature<'a>> {
        let span = self.start_span();
        let readonly = self.at(Kind::Readonly) && self.is_next_at_type_member_name();

        if readonly {
            self.bump_any();
        }

        let (key, computed) = self.parse_property_name()?;
        let optional = self.eat(Kind::Question);

        if self.at(Kind::LParen) || self.at(Kind::LAngle) {
            let TSSignature::TSCallSignatureDeclaration(call_signature) =
                self.parse_ts_call_signature_member()?
            else {
                unreachable!()
            };
            self.parse_type_member_semicolon();
            let call_signature = call_signature.unbox();
            Ok(self.ast.ts_signature_method_signature(
                self.end_span(span),
                key,
                computed,
                optional,
                TSMethodSignatureKind::Method,
                call_signature.type_parameters,
                call_signature.this_param,
                call_signature.params,
                call_signature.return_type,
            ))
        } else {
            let type_annotation = self.parse_ts_type_annotation()?;
            self.parse_type_member_semicolon();
            Ok(self.ast.ts_signature_property_signature(
                self.end_span(span),
                computed,
                optional,
                readonly,
                key,
                type_annotation,
            ))
        }
    }

    pub(crate) fn parse_ts_constructor_signature_member(&mut self) -> Result<TSSignature<'a>> {
        let span = self.start_span();
        self.expect(Kind::New)?;

        let type_parameters = self.parse_ts_type_parameters()?;
        let (this_param, params) = self.parse_formal_parameters(FormalParameterKind::Signature)?;

        if let Some(this_param) = this_param {
            // interface Foo { new(this: number): Foo }
            self.error(diagnostics::ts_constructor_this_parameter(this_param.span));
        }

        let return_type = self.parse_ts_return_type_annotation(Kind::Colon, false)?;
        self.parse_type_member_semicolon();

        Ok(self.ast.ts_signature_construct_signature_declaration(
            self.end_span(span),
            type_parameters,
            params,
            return_type,
        ))
    }

    pub(crate) fn parse_index_signature_declaration(
        &mut self,
        span: Span,
        modifiers: &Modifiers<'a>,
    ) -> Result<TSIndexSignature<'a>> {
        self.verify_modifiers(
            modifiers,
            ModifierFlags::READONLY | ModifierFlags::STATIC,
            diagnostics::cannot_appear_on_an_index_signature,
        );
        self.bump(Kind::LBrack);
        let parameters = self.ast.vec1(self.parse_ts_index_signature_name()?);
        self.expect(Kind::RBrack)?;
        let Some(type_annotation) = self.parse_ts_type_annotation()? else {
            return Err(self.unexpected());
        };
        self.parse_type_member_semicolon();
        Ok(self.ast.ts_index_signature(
            self.end_span(span),
            parameters,
            type_annotation,
            modifiers.contains(ModifierKind::Readonly),
            modifiers.contains(ModifierKind::Static),
        ))
    }

    fn parse_type_member_semicolon(&mut self) {
        // We allow type members to be separated by commas or (possibly ASI) semicolons.
        // First check if it was a comma.  If so, we're done with the member.
        if self.eat(Kind::Comma) {
            return;
        }
        // Didn't have a comma.  We must have a (possible ASI) semicolon.
        self.bump(Kind::Semicolon);
    }

    fn parse_ts_index_signature_name(&mut self) -> Result<TSIndexSignatureName<'a>> {
        let span = self.start_span();
        let name = self.parse_identifier_name()?.name;
        let type_annotation = self.parse_ts_type_annotation()?;

        if type_annotation.is_none() {
            return Err(self.unexpected());
        }

        Ok(self.ast.ts_index_signature_name(self.end_span(span), name, type_annotation.unwrap()))
    }

    pub(crate) fn parse_class_element_modifiers(
        &mut self,
        is_constructor_parameter: bool,
    ) -> Modifiers<'a> {
        if !self.is_ts {
            return Modifiers::empty();
        }

        let mut flags = ModifierFlags::empty();
        let mut modifiers: Vec<Modifier> = self.ast.vec();

        loop {
            if !self.is_nth_at_modifier(0, is_constructor_parameter) {
                break;
            }

            if let Ok(kind) = ModifierKind::try_from(self.cur_kind()) {
                let modifier = Modifier { kind, span: self.cur_token().span() };
                let new_flag = ModifierFlags::from(kind);
                if flags.contains(new_flag) {
                    self.error(diagnostics::accessibility_modifier_already_seen(&modifier));
                } else {
                    flags.insert(new_flag);
                    modifiers.push(modifier);
                }
            } else {
                break;
            }

            self.bump_any();
        }

        Modifiers::new(modifiers, flags)
    }

    fn parse_js_doc_unknown_or_nullable_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // bump `?`
        let type_annotation = self.parse_ts_type()?;
        let span = self.end_span(span);
        if matches!(
            self.cur_kind(),
            Kind::Comma | Kind::RCurly | Kind::RParen | Kind::RAngle | Kind::Eq | Kind::Pipe
        ) {
            Ok(self.ast.ts_type_js_doc_unknown_type(span))
        } else {
            Ok(self.ast.ts_type_js_doc_nullable_type(
                span,
                type_annotation,
                /* postfix */ false,
            ))
        }
    }

    fn parse_js_doc_non_nullable_type(&mut self) -> Result<TSType<'a>> {
        let span = self.start_span();
        self.bump_any(); // bump `!`
        let ty = self.parse_non_array_type()?;
        Ok(self.ast.ts_type_js_doc_non_nullable_type(
            self.end_span(span),
            ty,
            /* postfix */ false,
        ))
    }

    fn is_binary_operator(&self) -> bool {
        if self.ctx.has_in() && self.at(Kind::In) {
            return false;
        }
        self.cur_kind().is_binary_operator()
    }

    fn is_start_of_expression(&mut self) -> bool {
        if self.is_start_of_left_hand_side_expression() {
            return true;
        }
        match self.cur_kind() {
            kind if kind.is_unary_operator() => true,
            kind if kind.is_update_operator() => true,
            Kind::LAngle | Kind::Await | Kind::Yield | Kind::Private | Kind::At => true,
            kind if kind.is_binary_operator() => true,
            kind => kind.is_ts_identifier(self.ctx.has_yield(), self.ctx.has_await()),
        }
    }

    fn is_start_of_left_hand_side_expression(&mut self) -> bool {
        match self.cur_kind() {
            kind if kind.is_literal() => true,
            kind if kind.is_template_start_of_tagged_template() => true,
            Kind::This
            | Kind::Super
            | Kind::LParen
            | Kind::LBrack
            | Kind::LCurly
            | Kind::Function
            | Kind::Class
            | Kind::New
            | Kind::Slash
            | Kind::SlashEq => true,
            Kind::Import => {
                matches!(self.peek_kind(), Kind::LParen | Kind::LAngle | Kind::Dot)
            }
            _ => false,
        }
    }
}
