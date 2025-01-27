use crate::*;


static DEBUG_CANDIDATE_RESOLUTION: bool = false;


pub struct Assembler
{
	pub root_files: Vec<String>,
	pub state: State,
}


pub struct State
{
	pub is_first_pass: bool,
	pub banks: Vec<asm::Bank>,
	pub bankdata: Vec<asm::BankData>,
	pub symbols: asm::SymbolManager,
	pub symbol_guesses: asm::SymbolManager,
	pub rulesets: Vec<asm::Ruleset>,
	pub active_rulesets: Vec<RulesetRef>,
	pub cur_bank: BankRef,
	pub cur_wordsize: usize,
	pub cur_labelalign: usize,
}


#[derive(Clone, Debug)]
pub struct Context
{
	pub bit_offset: usize,
	pub cur_wordsize: usize,
	pub bank_ref: BankRef,
	pub symbol_ctx: asm::SymbolContext,
	pub cur_filename: std::rc::Rc<String>,
}


#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct BankRef
{
	pub index: usize,
}


#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RulesetRef
{
	pub index: usize,
}


#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RuleRef
{
	pub ruleset_ref: RulesetRef,
	pub index: usize,
}


pub struct AssemblyOutput
{
	pub binary: util::BitVec,
	pub symbols: asm::SymbolManager,
}


impl Assembler
{
	pub fn new() -> Assembler
	{
		Assembler
		{
			root_files: Vec::new(),
			state: State::new(),
		}
	}
	
	
	pub fn register_file<S: Into<String>>(
        &mut self,
        filename: S)
	{
		self.root_files.push(filename.into());
	}
	
	
	pub fn assemble(
        &mut self,
        report: diagn::RcReport,
		fileserver: &dyn util::FileServer,
		max_iterations: usize)
        -> Result<AssemblyOutput, ()>
	{
		let mut symbol_guesses = asm::SymbolManager::new();

		let mut iteration = 0;
		loop
		{
			self.state = State::new();
			self.state.is_first_pass = iteration == 0;
			std::mem::swap(&mut self.state.symbol_guesses, &mut symbol_guesses);

			iteration += 1;
			//dbg!(iteration);

			//dbg!(&symbol_guesses);
			//dbg!(&self.state.symbols);

			let pass_report = diagn::RcReport::new();

			for filename in &self.root_files
			{
				let result = asm::parser::parse_file(
					pass_report.clone(),
					&mut self.state,
					fileserver,
					filename,
					None);
				
				if pass_report.has_errors() || result.is_err()
				{
					pass_report.transfer_to(report);
					return Err(());
				}
			}

			//dbg!(&self.state.symbols);
			//dbg!(pass_report.has_errors());

			let mut full_output = util::BitVec::new();
			let mut all_bankdata_resolved = true;

			for bank_index in 0..self.state.banks.len()
			{
				let bank = &self.state.banks[bank_index];
				let bankdata = &self.state.bankdata[bank_index];

				let bank_output = self.state.resolve_bankdata(
					pass_report.clone(),
					bank,
					bankdata,
					fileserver);

				if pass_report.has_errors() || !bank_output.is_ok()
				{
					all_bankdata_resolved = false;
					break;
				}

				if let Some(output_offset) = bank.output_offset
				{
					//println!("output {:?}, {:x}", bank.output_offset, &bank_output.as_ref().unwrap());

					full_output.write_bitvec(
						output_offset,
						&bank_output.unwrap());
				}
				else
				{
					full_output.mark_spans_from(
						0,
						&bank_output.unwrap());
				}
			}

			if all_bankdata_resolved
			{
				pass_report.transfer_to(report);

				return Ok(AssemblyOutput
				{
					binary: full_output,
					symbols: std::mem::replace(&mut self.state.symbols, asm::SymbolManager::new()),
				});
			}

			if iteration >= max_iterations
			{
				pass_report.transfer_to(report);
				return Err(());				
			}

			std::mem::swap(&mut symbol_guesses, &mut self.state.symbols);
		}
	}
}


impl State
{
	pub fn new() -> State
	{
		let mut state = State
		{
			is_first_pass: false,
			banks: Vec::new(),
			bankdata: Vec::new(),
			symbols: asm::SymbolManager::new(),
			symbol_guesses: asm::SymbolManager::new(),
			rulesets: Vec::new(),
			active_rulesets: Vec::new(),
			cur_bank: BankRef { index: 0 },
			cur_wordsize: 8,
			cur_labelalign: 0,
		};

		state.create_bank(asm::Bank::new_default(), diagn::RcReport::new()).unwrap();

		state
	}


	pub fn get_ctx(&self, state: &asm::parser::State) -> Context
	{
		let bit_offset = self.get_bankdata(self.cur_bank).cur_bit_offset;
		let cur_wordsize = self.cur_wordsize;
		let bank_ref = self.cur_bank;
		let symbol_ctx = self.symbols.get_ctx();
		let cur_filename = state.filename.clone();

		Context
		{
			bit_offset,
			cur_wordsize,
			bank_ref,
			symbol_ctx,
			cur_filename,
		}
	}
	
	
	pub fn get_addr(&self, report: diagn::RcReport, ctx: &Context, span: &diagn::Span) -> Result<util::BigInt, ()>
	{
		let bank = &self.banks[ctx.bank_ref.index];
		let wordsize = ctx.cur_wordsize;
		
		let excess_bits = ctx.bit_offset % wordsize;
		if excess_bits != 0
		{
			let bits_short = wordsize - excess_bits;
			let plural = if bits_short > 1 { "bits" } else { "bit" };
			report.error_span(
				format!(
					"position is not aligned to an address boundary ({} {} short)",
					bits_short, plural),
				span);

			return Err(());
		}
			
		let addr =
			&util::BigInt::from(ctx.bit_offset / wordsize) +
			&bank.addr_start;
		
		Ok(addr)
	}
	
	
	pub fn get_addr_aprox(&self, ctx: &Context) -> util::BigInt
	{
		let bank = &self.banks[ctx.bank_ref.index];
		let wordsize = ctx.cur_wordsize;
			
		let addr =
			&util::BigInt::from(ctx.bit_offset / wordsize) +
			&bank.addr_start;
		
		addr
	}


	pub fn create_bank(
		&mut self,
		bank: asm::Bank,
		report: diagn::RcReport)
		-> Result<(), ()>
	{
		if self.banks.len() > 0 && self.bankdata[0].cur_bit_offset != 0
		{
			report.error_span(
				"cannot create new bank if the default bank has already been used",
				&bank.decl_span.as_ref().unwrap());

			return Err(());
		}

		if bank.output_offset.is_some()
		{
			for j in 1..self.banks.len()
			{
				let other_bank = &self.banks[j];

				if other_bank.output_offset.is_none()
					{ continue; }

				// FIXME: multiplication by wordsize can overflow
				let outp1 = bank.output_offset.unwrap();
				let outp2 = other_bank.output_offset.unwrap();

				// FIXME: multiplication by wordsize can overflow
				let size1 = bank.addr_size.map(|s| s * bank.wordsize);
				let size2 = other_bank.addr_size.map(|s| s * other_bank.wordsize);

				let overlap = match (size1, size2)
				{
					(None, None) => true,
					(Some(size1), None) => outp1 + size1 > outp2,
					(None, Some(size2)) => outp2 + size2 > outp1,
					(Some(size1), Some(size2)) => outp1 + size1 > outp2 && outp2 + size2 > outp1,
				};

				if overlap
				{
					report.error_span(
						format!(
							"output region overlaps with bank `{}`",
							other_bank.name),
						&bank.decl_span.as_ref().unwrap());

					return Err(());
				}
			}
		}

		let bank_ref = BankRef { index: self.banks.len() };

		self.cur_bank = bank_ref;
		self.cur_wordsize = bank.wordsize;
		self.cur_labelalign = bank.labelalign;

		self.banks.push(bank);

		self.bankdata.push(asm::BankData
		{
			bank_ref,
			cur_bit_offset: 0,
			invocations: Vec::new(),
		});

		Ok(())
	}


	pub fn find_bank<TName: std::borrow::Borrow<str>>(
		&self,
		name: TName,
		report: diagn::RcReport,
		span: &diagn::Span)
		-> Result<BankRef, ()>
	{
		match self.banks.iter().position(|rg| rg.name == name.borrow())
		{
			Some(index) => Ok(BankRef{ index }),
			None =>
			{
				report.error_span("unknown bank", span);
				Err(())
			}
		}
	}


	pub fn get_bankdata(
		&self,
		bank_ref: BankRef)
		-> &asm::BankData
	{
		&self.bankdata[bank_ref.index]
	}


	pub fn get_bankdata_mut(
		&mut self,
		bank_ref: BankRef)
		-> &mut asm::BankData
	{
		&mut self.bankdata[bank_ref.index]
	}


	pub fn find_ruleset<TName: std::borrow::Borrow<str>>(
		&self,
		name: TName,
		report: diagn::RcReport,
		span: &diagn::Span)
		-> Result<RulesetRef, ()>
	{
		match self.rulesets.iter().position(|rg| rg.name == name.borrow())
		{
			Some(index) => Ok(RulesetRef{ index }),
			None =>
			{
				report.error_span("unknown ruleset", span);
				Err(())
			}
		}
	}
	

	pub fn activate_ruleset<TName: std::borrow::Borrow<str>>(
		&mut self,
		name: TName,
		report: diagn::RcReport,
		span: &diagn::Span)
		-> Result<(), ()>
	{
		let rg_ref = self.find_ruleset(name.borrow(), report, span)?;

		self.active_rulesets.push(rg_ref);
		Ok(())
	}
	

	pub fn get_rule(
		&self,
		rule_ref: asm::RuleRef)
		-> Option<&asm::Rule>
	{
		Some(&self.rulesets[rule_ref.ruleset_ref.index].rules[rule_ref.index])
	}


	pub fn resolve_bankdata(
		&self,
		report: diagn::RcReport,
		bank: &asm::Bank,
		bankdata: &asm::BankData,
		fileserver: &dyn util::FileServer)
		-> Result<util::BitVec, ()>
	{
		let mut bitvec = util::BitVec::new();

		for invoc in &bankdata.invocations
		{
			let resolved = match invoc.kind
			{
				asm::InvocationKind::Rule(_) =>
				{
					let _guard = report.push_parent(
						"failed to resolve instruction",
						&invoc.span);
			
					self.resolve_rule_invocation(
						report.clone(),
						&invoc,
						fileserver,
						true)?
				}
				
				asm::InvocationKind::Data(_) =>
				{
					let _guard = report.push_parent(
						"failed to resolve data element",
						&invoc.span);
			
					self.resolve_data_invocation(
						report.clone(),
						&invoc,
						fileserver,
						true)?
				}
				
				asm::InvocationKind::Label(_) =>
				{
					let offset = if bank.output_offset.is_some()
					{
						Some(invoc.ctx.bit_offset)
					}
					else
					{
						None
					};

					bitvec.mark_span(
						offset,
						0,
						self.get_addr_aprox(&invoc.ctx),
						invoc.span.clone());

					continue;
				}
			};

			let expr_name = match invoc.kind
			{
				asm::InvocationKind::Rule(_) => "instruction",
				asm::InvocationKind::Data(_) => "data element",
				_ => unreachable!(),
			};

			let (bigint, size) = match resolved
			{
				expr::Value::Integer(bigint) =>
				{
					match bigint.size
					{
						Some(size) =>
						{
							if size == invoc.size_guess
							{
								(bigint, size)
							}
							else
							{
								report.error_span(
									format!(
										"{} size did not converge after iterations",
										expr_name),
									&invoc.span);

								continue;
							}
						}
						None =>
						{
							report.error_span(
								format!(
									"cannot infer size of {}",
									expr_name),
								&invoc.span);

							continue;
						}
					}
				}

				_ =>
				{
					report.error_span(
						format!(
							"wrong type returned from {}",
							expr_name),
						&invoc.span);

					continue;
				}
			};

			if let Some(addr_size) = bank.addr_size
			{
				if invoc.ctx.bit_offset + size > addr_size * bank.wordsize
				{
					report.error_span(
						format!(
							"{} is out of bank range",
							expr_name),
						&invoc.span);

					continue;
				}
			}
			
			bitvec.write_bigint(invoc.ctx.bit_offset, bigint);
			bitvec.mark_span(
				Some(invoc.ctx.bit_offset),
				size,
				self.get_addr_aprox(&invoc.ctx),
				invoc.span.clone());
		}

		if bank.fill && bank.addr_size.is_some()
		{
			while bitvec.len() < bank.addr_size.unwrap() * bank.wordsize
			{
				bitvec.write(bitvec.len(), false);
			}
		}

		Ok(bitvec)
	}


	pub fn resolve_data_invocation(
		&self,
		report: diagn::RcReport,
		invocation: &asm::Invocation,
		fileserver: &dyn util::FileServer,
		final_pass: bool)
		-> Result<expr::Value, ()>
	{
		let data_invoc = &invocation.get_data_invoc();

		let mut resolved = self.eval_expr(
			report.clone(),
			&data_invoc.expr,
			&invocation.ctx,
			&mut expr::EvalContext::new(),
			fileserver,
			final_pass)?;

		if let Some(elem_size) = data_invoc.elem_size
		{
			match resolved
			{
				expr::Value::Integer(ref mut bigint) =>
				{
					let mut size = bigint.min_size();
					if let Some(intrinsic_size) = bigint.size
					{
						size = intrinsic_size;
					}
					
					if size > elem_size
					{
						report.error_span(
							format!(
								"value size (= {}) is larger than the directive size (= {})",
								size,
								elem_size),
							&data_invoc.expr.span());
					}

					bigint.size = Some(elem_size);
				}
				_ => {}
			}
		}

		Ok(resolved)
	}


	pub fn resolve_rule_invocation(
		&self,
		report: diagn::RcReport,
		invocation: &asm::Invocation,
		fileserver: &dyn util::FileServer,
		final_pass: bool)
		-> Result<expr::Value, ()>
	{
		self.resolve_rule_invocation_candidates(
			report.clone(),
			invocation,
			&invocation.get_rule_invoc().candidates,
			fileserver,
			final_pass)
	}


	pub fn resolve_rule_invocation_candidates(
		&self,
		report: diagn::RcReport,
		invocation: &asm::Invocation,
		candidates: &Vec<asm::RuleInvocationCandidate>,
		fileserver: &dyn util::FileServer,
		final_pass: bool)
		-> Result<expr::Value, ()>
	{
		if DEBUG_CANDIDATE_RESOLUTION
		{
			println!(
				"=== resolve candidates for invocation `{}` ===",
				fileserver.get_excerpt(&invocation.span));
		}

		if final_pass && candidates.len() == 1
		{
			return self.resolve_rule_invocation_candidate(
				report,
				invocation,
				&candidates[0],
				fileserver,
				final_pass)
		}

		let mut successful_candidates = Vec::new();

		for candidate in candidates
		{
			let candidate_report = diagn::RcReport::new();

			if DEBUG_CANDIDATE_RESOLUTION
			{
                let rule_group = &self.rulesets[candidate.rule_ref.ruleset_ref.index];
                let rule = &rule_group.rules[candidate.rule_ref.index];

				println!(
					"> try candidate `{}`",
					fileserver.get_excerpt(&rule.span));
			}

			match self.resolve_rule_invocation_candidate(
				candidate_report.clone(),
				invocation,
				candidate,
				fileserver,
				final_pass)
			{
				Ok(resolved) =>
				{
					if DEBUG_CANDIDATE_RESOLUTION
					{
						println!("  ok");
					}

					successful_candidates.push((candidate, resolved, candidate_report));
				}
				Err(()) => {}
			}
		}

		if successful_candidates.len() > 0
		{
			if final_pass
			{
				if successful_candidates.len() > 1
				{
					report.error_span(
						"multiple matches for instruction",
						&invocation.span);
					return Err(())
				}

				self.resolve_rule_invocation_candidate(
					report,
					invocation,
					successful_candidates[0].0,
					fileserver,
					final_pass)
			}
			else
			{
				successful_candidates.last().unwrap().2.transfer_to(report);
				Ok(successful_candidates.last().unwrap().1.clone())
			}
		}
		else
		{
			Err(())
		}
	}


	pub fn resolve_rule_invocation_candidate(
		&self,
		report: diagn::RcReport,
		invocation: &asm::Invocation,
		candidate: &asm::RuleInvocationCandidate,
		fileserver: &dyn util::FileServer,
		final_pass: bool)
		-> Result<expr::Value, ()>
	{
		let rule = self.get_rule(candidate.rule_ref).unwrap();

		let mut eval_ctx = expr::EvalContext::new();
		for (arg_index, arg) in candidate.args.iter().enumerate()
		{
			match arg
			{
				&asm::RuleInvocationArgument::Expression(ref expr) =>
				{
					let mut arg_value = self.eval_expr(
						report.clone(),
						&expr,
						&invocation.ctx,
						&mut expr::EvalContext::new(),
						fileserver,
						final_pass)?;

					let arg = &rule.parameters[arg_index];

					State::check_and_constrain_argument(
						&mut arg_value,
						arg.typ,
						report.clone(),
						&expr.span())?;

					eval_ctx.set_local(&arg.name, arg_value);
				}

				&asm::RuleInvocationArgument::NestedRuleset(ref inner_candidates) =>
				{
					let arg_value = self.resolve_rule_invocation_candidates(
						report.clone(),
						invocation,
						&inner_candidates,
						fileserver,
						final_pass)?;

					let arg_name = &rule.parameters[arg_index].name;

					eval_ctx.set_local(arg_name, arg_value);
				}
			}
		}

		self.eval_expr(
			report,
			&rule.production,
			&invocation.ctx,
			&mut eval_ctx,
			fileserver,
			final_pass)
	}


	pub fn check_and_constrain_argument(
		value: &mut expr::Value,
		typ: asm::PatternParameterType,
		report: diagn::RcReport,
		span: &diagn::Span)
		-> Result<(), ()>
	{
		match typ
		{
			asm::PatternParameterType::Unspecified => Ok(()),
			asm::PatternParameterType::Ruleset(_) => unreachable!(),

			asm::PatternParameterType::Unsigned(size) =>
			{
				if let expr::Value::Integer(value_int) = value
				{
					if value_int.sign() == -1 ||
						value_int.min_size() > size
					{
						report.error_span(
							&format!("argument out of range for type `u{}`", size),
							&span);
						Err(())
					}
					else
					{
						value_int.size = Some(size);
						Ok(())
					}
				}
				else
				{
					report.error_span(
						&format!("wrong argument for type `u{}`", size),
						&span);
					Err(())
				}
			}

			asm::PatternParameterType::Signed(size) =>
			{
				if let expr::Value::Integer(value_int) = value
				{
					if (value_int.sign() == 0 && size == 0) ||
						(value_int.sign() == 1 && value_int.min_size() >= size) ||
						(value_int.sign() == -1 && value_int.min_size() > size)
					{
						report.error_span(
							&format!("argument out of range for type `s{}`", size),
							&span);
						Err(())
					}
					else
					{
						value_int.size = Some(size);
						Ok(())
					}
				}
				else
				{
					report.error_span(
						&format!("wrong argument for type `s{}`", size),
						&span);
					Err(())
				}
			}

			asm::PatternParameterType::Integer(size) =>
			{
				if let expr::Value::Integer(value_int) = value
				{
					if value_int.min_size() > size
					{
						report.error_span(
							&format!("argument out of range for type `i{}`", size),
							&span);
						Err(())
					}
					else
					{
						value_int.size = Some(size);
						Ok(())
					}
				}
				else
				{
					report.error_span(
						&format!("wrong argument for type `i{}`", size),
						&span);
					Err(())
				}
			}
		}
	}
	

	pub fn eval_expr(
		&self,
		report: diagn::RcReport,
		expr: &expr::Expr,
		ctx: &Context,
		eval_ctx: &mut expr::EvalContext,
		fileserver: &dyn util::FileServer,
		final_pass: bool)
		-> Result<expr::Value, ()>
	{
		expr.eval(
			report,
			eval_ctx,
			&|info| self.eval_var(ctx, info, fileserver, final_pass),
			&|info| self.eval_fn(ctx, info, fileserver))
	}
	
		
	fn eval_var(
		&self,
		ctx: &Context,
		info: &expr::EvalVariableInfo,
		_fileserver: &dyn util::FileServer,
		final_pass: bool)
		-> Result<expr::Value, bool>
	{
		if info.hierarchy_level == 0 && info.hierarchy.len() == 1
		{
			match info.hierarchy[0].as_ref()
			{
				"$" | "pc" =>
				{
					return match self.get_addr(
						info.report.clone(),
						&ctx,
						&info.span)
					{
						Err(()) => Err(true),
						Ok(addr) => Ok(expr::Value::make_integer(addr))
					};
				}

				"assert" |
				"incbin" |
				"incbinstr" |
				"inchexstr"  =>
				{
					return Ok(expr::Value::Function(info.hierarchy[0].clone()));
				}

				_ => {}
			}
		}

		//println!("reading hierarchy level {}, hierarchy {:?}, ctx {:?}", info.hierarchy_level, info.hierarchy, &ctx.symbol_ctx);

		if let Some(symbol) = self.symbols.get(&ctx.symbol_ctx, info.hierarchy_level, info.hierarchy)
		{
			Ok(symbol.value.clone())
		}
		else if !final_pass
		{
			if let Some(symbol) = self.symbol_guesses.get(&ctx.symbol_ctx, info.hierarchy_level, info.hierarchy)
			{
				Ok(symbol.value.clone())
			}
			else if self.is_first_pass
			{
				Ok(expr::Value::make_integer(0))
			}
			else
			{
				Err(false)
			}
		}
		else
		{
			Err(false)
		}
	}


	fn eval_fn(
		&self,
		ctx: &Context,
		info: &expr::EvalFunctionInfo,
		fileserver: &dyn util::FileServer)
		-> Result<expr::Value, bool>
	{
		match info.func
		{
			expr::Value::Function(ref name) =>
			{
				match name.as_ref()
				{
					"assert" =>
					{
						if info.args.len() != 1
						{
							info.report.error_span("wrong number of arguments", info.span);
							return Err(true);
						}
							
						match info.args[0]
						{
							expr::Value::Bool(value) =>
							{
								match value
								{
									true => Ok(expr::Value::Void),
									false =>
									{
										info.report.error_span("assertion failed", info.span);
										return Err(true);
									}
								}
							}
							
							_ =>
							{
								info.report.error_span("wrong argument type", info.span);
								return Err(true);
							}
						}
					}

					"incbin" |
					"incbinstr" |
					"inchexstr" =>
					{
						match &info.args[..]
						{
							&[expr::Value::Integer(ref bigint)] =>
							{
								let filename = bigint.as_string();
								let new_filename = util::filename_navigate(
									info.report.clone(),
									&ctx.cur_filename,
									&filename,
									&info.span)
									.map_err(|_| true)?;

								match name.as_ref()
								{
									"incbin" =>
									{
										let bytes = fileserver.get_bytes(
											info.report.clone(),
											&new_filename,
											Some(&info.span))
											.map_err(|_| true)?;

										Ok(expr::Value::make_integer(util::BigInt::from_bytes_be(&bytes)))
									}

									"incbinstr" |
									"inchexstr" =>
									{
										let chars = fileserver.get_chars(
											info.report.clone(),
											&new_filename,
											Some(&info.span))
											.map_err(|_| true)?;

										let mut bitvec = util::BitVec::new();

										let bits_per_char = match name.as_ref()
										{
											"incbinstr" => 1,
											"inchexstr" => 4,
											_ => unreachable!(),
										};
										
										for c in chars
										{
											if syntax::is_whitespace(c) ||
												c == '_' ||
												c == '\r' || c == '\n'
											{
												continue;
											}

											let digit = match c.to_digit(1 << bits_per_char)
											{
												Some(digit) => digit,
												None =>
												{
													info.report.error_span(
														"invalid character in file contents",
														&info.span);
													return Err(true);
												}
											};
											
											for i in 0..bits_per_char
											{
												let bit = (digit & (1 << (bits_per_char - 1 - i))) != 0;
												bitvec.write(bitvec.len(), bit);
											}
										}

										// TODO: Optimize conversion to bigint
										Ok(expr::Value::make_integer(bitvec.as_bigint()))
									}

									_ => unreachable!()
								}

							}
							
							_ =>
							{
								info.report.error_span("wrong arguments", info.span);
								return Err(true);
							}
						}
					}

					_ => unreachable!()
				}
			}
			
			_ => unreachable!()
		}
	}
}
