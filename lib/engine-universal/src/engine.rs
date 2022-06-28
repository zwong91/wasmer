//! Universal compilation.

use crate::code_memory::{
    CodeMemory, LimitedMemoryPool, ARCH_FUNCTION_ALIGNMENT, DATA_SECTION_ALIGNMENT,
};
use crate::executable::{unrkyv, UniversalExecutableRef};
use crate::{UniversalArtifact, UniversalExecutable};
use rkyv::de::deserializers::SharedDeserializeMap;
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::sync::{Arc, Mutex};
#[cfg(feature = "compiler")]
use wasmer_compiler::Compiler;
use wasmer_compiler::{
    CompileError, CompiledFunctionUnwindInfoRef, CustomSectionProtection, CustomSectionRef,
    FunctionBodyRef, JumpTable, SectionIndex, Target,
};
use wasmer_engine::{Engine, EngineId};
use wasmer_types::entity::{EntityRef, PrimaryMap};
use wasmer_types::{
    DataInitializer, ExportIndex, Features, FunctionIndex, FunctionType, FunctionTypeRef,
    GlobalInit, GlobalType, ImportCounts, ImportIndex, LocalFunctionIndex, LocalGlobalIndex,
    MemoryIndex, SignatureIndex, TableIndex,
};
use wasmer_vm::{
    FuncDataRegistry, FunctionBodyPtr, SectionBodyPtr, SignatureRegistry, Tunables,
    VMCallerCheckedAnyfunc, VMFuncRef, VMImportType, VMLocalFunction, VMOffsets,
    VMSharedSignatureIndex, VMTrampoline,
};

/// A WebAssembly `Universal` Engine.
#[derive(Clone)]
pub struct UniversalEngine {
    inner: Arc<Mutex<UniversalEngineInner>>,
    /// The target for the compiler
    target: Arc<Target>,
    engine_id: EngineId,
}

impl UniversalEngine {
    /// Create a new `UniversalEngine` with the given config
    #[cfg(feature = "compiler")]
    pub fn new(
        compiler: Box<dyn Compiler>,
        target: Target,
        features: Features,
        memory_allocator: crate::code_memory::LimitedMemoryPool,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(UniversalEngineInner {
                compiler: Some(compiler),
                code_memory_pool: memory_allocator,
                signatures: SignatureRegistry::new(),
                func_data: Arc::new(FuncDataRegistry::new()),
                features,
            })),
            target: Arc::new(target),
            engine_id: EngineId::default(),
        }
    }

    /// Create a headless `UniversalEngine`
    ///
    /// A headless engine is an engine without any compiler attached.
    /// This is useful for assuring a minimal runtime for running
    /// WebAssembly modules.
    ///
    /// For example, for running in IoT devices where compilers are very
    /// expensive, or also to optimize startup speed.
    ///
    /// # Important
    ///
    /// Headless engines can't compile or validate any modules,
    /// they just take already processed Modules (via `Module::serialize`).
    pub fn headless(memory_allocator: crate::code_memory::LimitedMemoryPool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(UniversalEngineInner {
                #[cfg(feature = "compiler")]
                compiler: None,
                signatures: SignatureRegistry::new(),
                func_data: Arc::new(FuncDataRegistry::new()),
                features: Features::default(),
                code_memory_pool: memory_allocator,
            })),
            target: Arc::new(Target::default()),
            engine_id: EngineId::default(),
        }
    }

    pub(crate) fn inner(&self) -> std::sync::MutexGuard<'_, UniversalEngineInner> {
        self.inner.lock().unwrap()
    }

    pub(crate) fn inner_mut(&self) -> std::sync::MutexGuard<'_, UniversalEngineInner> {
        self.inner.lock().unwrap()
    }

    /// Compile a WebAssembly binary
    #[cfg(feature = "compiler")]
    #[tracing::instrument(skip_all)]
    pub fn compile_universal(
        &self,
        binary: &[u8],
        tunables: &dyn Tunables,
    ) -> Result<crate::UniversalExecutable, CompileError> {
        let inner_engine = self.inner_mut();
        let features = inner_engine.features();
        let compiler = inner_engine.compiler()?;
        let environ = wasmer_compiler::ModuleEnvironment::new();
        let translation = environ.translate(binary).map_err(CompileError::Wasm)?;

        let memory_styles: PrimaryMap<wasmer_types::MemoryIndex, _> = translation
            .module
            .memories
            .values()
            .map(|memory_type| tunables.memory_style(memory_type))
            .collect();
        let table_styles: PrimaryMap<wasmer_types::TableIndex, _> = translation
            .module
            .tables
            .values()
            .map(|table_type| tunables.table_style(table_type))
            .collect();

        // Compile the Module
        let compile_info = wasmer_compiler::CompileModuleInfo {
            module: Arc::new(translation.module),
            features: features.clone(),
            memory_styles,
            table_styles,
        };
        let compilation = compiler.compile_module(
            &self.target(),
            &compile_info,
            // SAFETY: Calling `unwrap` is correct since
            // `environ.translate()` above will write some data into
            // `module_translation_state`.
            translation.module_translation_state.as_ref().unwrap(),
            translation.function_body_inputs,
        )?;
        let function_call_trampolines = compilation.get_function_call_trampolines();
        let dynamic_function_trampolines = compilation.get_dynamic_function_trampolines();
        let data_initializers = translation
            .data_initializers
            .iter()
            .map(wasmer_types::OwnedDataInitializer::new)
            .collect();

        let frame_infos = compilation.get_frame_info();
        Ok(crate::UniversalExecutable {
            function_bodies: compilation.get_function_bodies(),
            function_relocations: compilation.get_relocations(),
            function_jt_offsets: compilation.get_jt_offsets(),
            function_frame_info: frame_infos,
            function_call_trampolines,
            dynamic_function_trampolines,
            custom_sections: compilation.get_custom_sections(),
            custom_section_relocations: compilation.get_custom_section_relocations(),
            debug: compilation.get_debug(),
            trampolines: compilation.get_trampolines(),
            compile_info,
            data_initializers,
            cpu_features: self.target().cpu_features().as_u64(),
        })
    }

    /// Load a [`UniversalExecutable`](crate::UniversalExecutable) with this engine.
    #[tracing::instrument(skip_all)]
    pub fn load_universal_executable(
        &self,
        executable: &UniversalExecutable,
    ) -> Result<UniversalArtifact, CompileError> {
        let info = &executable.compile_info;
        let module = &info.module;
        let local_memories = (module.import_counts.memories as usize..module.memories.len())
            .map(|idx| {
                let idx = MemoryIndex::new(idx);
                (module.memories[idx], info.memory_styles[idx].clone())
            })
            .collect();
        let local_tables = (module.import_counts.tables as usize..module.tables.len())
            .map(|idx| {
                let idx = TableIndex::new(idx);
                (module.tables[idx], info.table_styles[idx].clone())
            })
            .collect();
        let local_globals: Vec<(GlobalType, GlobalInit)> = module
            .globals
            .iter()
            .skip(module.import_counts.globals as usize)
            .enumerate()
            .map(|(idx, (_, t))| {
                let init = module.global_initializers[LocalGlobalIndex::new(idx)];
                (*t, init)
            })
            .collect();
        let mut inner_engine = self.inner_mut();

        let local_functions = executable.function_bodies.iter().map(|(_, b)| b.into());
        let function_call_trampolines = &executable.function_call_trampolines;
        let dynamic_function_trampolines = &executable.dynamic_function_trampolines;
        let signatures = module
            .signatures
            .iter()
            .map(|(_, sig)| inner_engine.signatures.register(sig.into()))
            .collect::<PrimaryMap<SignatureIndex, _>>()
            .into_boxed_slice();
        let (functions, trampolines, dynamic_trampolines, custom_sections, mut code_memory) =
            inner_engine.allocate(
                local_functions,
                function_call_trampolines.iter().map(|(_, b)| b.into()),
                dynamic_function_trampolines.iter().map(|(_, b)| b.into()),
                executable.custom_sections.iter().map(|(_, s)| s.into()),
                |idx: LocalFunctionIndex| {
                    let func_idx = module.import_counts.function_index(idx);
                    let sig_idx = module.functions[func_idx];
                    (sig_idx, signatures[sig_idx])
                },
            )?;
        let imports = module
            .imports
            .iter()
            .map(|((module_name, field, idx), entity)| wasmer_vm::VMImport {
                module: String::from(module_name),
                field: String::from(field),
                import_no: *idx,
                ty: match entity {
                    ImportIndex::Function(i) => {
                        let sig_idx = module.functions[*i];
                        VMImportType::Function {
                            sig: signatures[sig_idx],
                            static_trampoline: trampolines[sig_idx],
                        }
                    }
                    ImportIndex::Table(i) => VMImportType::Table(module.tables[*i]),
                    &ImportIndex::Memory(i) => {
                        let ty = module.memories[i];
                        VMImportType::Memory(ty, info.memory_styles[i].clone())
                    }
                    ImportIndex::Global(i) => VMImportType::Global(module.globals[*i]),
                },
            })
            .collect();

        let function_relocations = executable.function_relocations.iter();
        let section_relocations = executable.custom_section_relocations.iter();
        crate::link_module(
            &functions,
            |func_idx, jt_idx| executable.function_jt_offsets[func_idx][jt_idx],
            function_relocations.map(|(i, rs)| (i, rs.iter().cloned())),
            &custom_sections,
            section_relocations.map(|(i, rs)| (i, rs.iter().cloned())),
            &executable.trampolines,
        );

        unsafe {
            // TODO:
            code_memory.publish();
        }
        let exports = module
            .exports
            .iter()
            .map(|(s, i)| (s.clone(), i.clone()))
            .collect::<BTreeMap<String, ExportIndex>>();
        Ok(UniversalArtifact {
            engine: self.clone(),
            _code_memory: code_memory,
            import_counts: module.import_counts,
            start_function: module.start_function,
            vmoffsets: VMOffsets::for_host().with_module_info(&*module),
            imports,
            dynamic_function_trampolines: dynamic_trampolines.into_boxed_slice(),
            functions: functions.into_boxed_slice(),
            exports,
            signatures,
            local_memories,
            data_segments: executable.data_initializers.clone(),
            passive_data: module.passive_data.clone(),
            local_tables,
            element_segments: module.table_initializers.clone(),
            passive_elements: module.passive_elements.clone(),
            local_globals,
        })
    }

    /// Load a [`UniversalExecutableRef`](crate::UniversalExecutableRef) with this engine.
    pub fn load_universal_executable_ref(
        &self,
        executable: &UniversalExecutableRef,
    ) -> Result<UniversalArtifact, CompileError> {
        let info = &executable.compile_info;
        let module = &info.module;
        let import_counts: ImportCounts = unrkyv(&module.import_counts);
        let local_memories = (import_counts.memories as usize..module.memories.len())
            .map(|idx| {
                let idx = MemoryIndex::new(idx);
                let mty = &module.memories[&idx];
                (unrkyv(mty), unrkyv(&info.memory_styles[&idx]))
            })
            .collect();
        let local_tables = (import_counts.tables as usize..module.tables.len())
            .map(|idx| {
                let idx = TableIndex::new(idx);
                let tty = &module.tables[&idx];
                (unrkyv(tty), unrkyv(&info.table_styles[&idx]))
            })
            .collect();
        let local_globals: Vec<(GlobalType, GlobalInit)> = module
            .globals
            .iter()
            .skip(import_counts.globals as _)
            .enumerate()
            .map(|(idx, (_, t))| {
                let init = unrkyv(&module.global_initializers[&LocalGlobalIndex::new(idx)]);
                (*t, init)
            })
            .collect();

        let passive_data =
            rkyv::Deserialize::deserialize(&module.passive_data, &mut SharedDeserializeMap::new())
                .map_err(|_| CompileError::Validate("could not deserialize passive data".into()))?;
        let data_segments = executable.data_initializers.iter();
        let data_segments = data_segments
            .map(|s| DataInitializer::from(s).into())
            .collect();
        let element_segments = unrkyv(&module.table_initializers);
        let passive_elements: BTreeMap<wasmer_types::ElemIndex, Box<[FunctionIndex]>> =
            unrkyv(&module.passive_elements);

        let import_counts: ImportCounts = unrkyv(&module.import_counts);
        let mut inner_engine = self.inner_mut();

        let local_functions = executable.function_bodies.iter().map(|(_, b)| b.into());
        let call_trampolines = executable.function_call_trampolines.iter();
        let dynamic_trampolines = executable.dynamic_function_trampolines.iter();
        let signatures = module
            .signatures
            .values()
            .map(|sig| inner_engine.signatures.register(sig.into()))
            .collect::<PrimaryMap<SignatureIndex, _>>()
            .into_boxed_slice();
        let (functions, trampolines, dynamic_trampolines, custom_sections, mut code_memory) =
            inner_engine.allocate(
                local_functions,
                call_trampolines.map(|(_, b)| b.into()),
                dynamic_trampolines.map(|(_, b)| b.into()),
                executable.custom_sections.iter().map(|(_, s)| s.into()),
                |idx: LocalFunctionIndex| {
                    let func_idx = import_counts.function_index(idx);
                    let sig_idx = module.functions[&func_idx];
                    (sig_idx, signatures[sig_idx])
                },
            )?;
        let imports = {
            module
                .imports
                .iter()
                .map(|((module_name, field, idx), entity)| wasmer_vm::VMImport {
                    module: String::from(module_name.as_str()),
                    field: String::from(field.as_str()),
                    import_no: *idx,
                    ty: match entity {
                        ImportIndex::Function(i) => {
                            let sig_idx = module.functions[i];
                            VMImportType::Function {
                                sig: signatures[sig_idx],
                                static_trampoline: trampolines[sig_idx],
                            }
                        }
                        ImportIndex::Table(i) => VMImportType::Table(unrkyv(&module.tables[i])),
                        ImportIndex::Memory(i) => {
                            let ty = unrkyv(&module.memories[i]);
                            VMImportType::Memory(ty, unrkyv(&info.memory_styles[i]))
                        }
                        ImportIndex::Global(i) => VMImportType::Global(unrkyv(&module.globals[i])),
                    },
                })
                .collect()
        };

        let function_relocations = executable.function_relocations.iter();
        let section_relocations = executable.custom_section_relocations.iter();
        crate::link_module(
            &functions,
            |func_idx, jt_idx| {
                let func_idx = rkyv::Archived::<LocalFunctionIndex>::new(func_idx.index());
                let jt_idx = rkyv::Archived::<JumpTable>::new(jt_idx.index());
                executable.function_jt_offsets[&func_idx][&jt_idx]
            },
            function_relocations.map(|(i, r)| (i, r.iter().map(unrkyv))),
            &custom_sections,
            section_relocations.map(|(i, r)| (i, r.iter().map(unrkyv))),
            &unrkyv(&executable.trampolines),
        );

        unsafe {
            // TODO:
            code_memory.publish();
        }
        let exports = module
            .exports
            .iter()
            .map(|(s, i)| (unrkyv(s), unrkyv(i)))
            .collect::<BTreeMap<String, ExportIndex>>();
        Ok(UniversalArtifact {
            engine: self.clone(),
            _code_memory: code_memory,
            import_counts,
            start_function: unrkyv(&module.start_function),
            vmoffsets: VMOffsets::for_host().with_archived_module_info(&*module),
            imports,
            dynamic_function_trampolines: dynamic_trampolines.into_boxed_slice(),
            functions: functions.into_boxed_slice(),
            exports,
            signatures,
            local_memories,
            data_segments,
            passive_data,
            local_tables,
            element_segments,
            passive_elements,
            local_globals,
        })
    }
}

impl Engine for UniversalEngine {
    /// The target
    fn target(&self) -> &Target {
        &self.target
    }

    /// Register a signature
    fn register_signature(&self, func_type: FunctionTypeRef<'_>) -> VMSharedSignatureIndex {
        self.inner().signatures.register(func_type)
    }

    fn register_function_metadata(&self, func_data: VMCallerCheckedAnyfunc) -> VMFuncRef {
        self.inner().func_data().register(func_data)
    }

    /// Lookup a signature
    fn lookup_signature(&self, sig: VMSharedSignatureIndex) -> Option<FunctionType> {
        self.inner().signatures.lookup(sig).cloned()
    }

    /// Validates a WebAssembly module
    #[tracing::instrument(skip_all)]
    fn validate(&self, binary: &[u8]) -> Result<(), CompileError> {
        self.inner().validate(binary)
    }

    fn id(&self) -> &EngineId {
        &self.engine_id
    }

    fn cloned(&self) -> Arc<dyn Engine + Send + Sync> {
        Arc::new(self.clone())
    }
}

/// The inner contents of `UniversalEngine`
pub struct UniversalEngineInner {
    /// The compiler
    #[cfg(feature = "compiler")]
    compiler: Option<Box<dyn Compiler>>,
    /// The features to compile the Wasm module with
    features: Features,
    /// Pool from which code memory can be allocated.
    code_memory_pool: LimitedMemoryPool,
    /// The signature registry is used mainly to operate with trampolines
    /// performantly.
    pub(crate) signatures: SignatureRegistry,
    /// The backing storage of `VMFuncRef`s. This centralized store ensures that 2
    /// functions with the same `VMCallerCheckedAnyfunc` will have the same `VMFuncRef`.
    /// It also guarantees that the `VMFuncRef`s stay valid until the engine is dropped.
    func_data: Arc<FuncDataRegistry>,
}

impl UniversalEngineInner {
    /// Gets the compiler associated to this engine.
    #[cfg(feature = "compiler")]
    pub fn compiler(&self) -> Result<&dyn Compiler, CompileError> {
        if self.compiler.is_none() {
            return Err(CompileError::Codegen("The UniversalEngine is operating in headless mode, so it can only execute already compiled Modules.".to_string()));
        }
        Ok(&**self.compiler.as_ref().unwrap())
    }

    /// Validate the module
    #[cfg(feature = "compiler")]
    pub fn validate<'data>(&self, data: &'data [u8]) -> Result<(), CompileError> {
        self.compiler()?.validate_module(self.features(), data)
    }

    /// Validate the module
    #[cfg(not(feature = "compiler"))]
    pub fn validate<'data>(&self, _data: &'data [u8]) -> Result<(), CompileError> {
        Err(CompileError::Validate(
            "The UniversalEngine is not compiled with compiler support, which is required for validating"
                .to_string(),
        ))
    }

    /// The Wasm features
    pub fn features(&self) -> &Features {
        &self.features
    }

    /// Allocate compiled functions into memory
    #[allow(clippy::type_complexity)]
    pub(crate) fn allocate<'a>(
        &mut self,
        local_functions: impl ExactSizeIterator<Item = FunctionBodyRef<'a>>,
        call_trampolines: impl ExactSizeIterator<Item = FunctionBodyRef<'a>>,
        dynamic_trampolines: impl ExactSizeIterator<Item = FunctionBodyRef<'a>>,
        custom_sections: impl ExactSizeIterator<Item = CustomSectionRef<'a>>,
        function_signature: impl Fn(LocalFunctionIndex) -> (SignatureIndex, VMSharedSignatureIndex),
    ) -> Result<
        (
            PrimaryMap<LocalFunctionIndex, VMLocalFunction>,
            PrimaryMap<SignatureIndex, VMTrampoline>,
            PrimaryMap<FunctionIndex, FunctionBodyPtr>,
            PrimaryMap<SectionIndex, SectionBodyPtr>,
            CodeMemory,
        ),
        CompileError,
    > {
        let code_memory_pool = &mut self.code_memory_pool;
        let function_count = local_functions.len();
        let call_trampoline_count = call_trampolines.len();
        let function_bodies = call_trampolines
            .chain(local_functions)
            .chain(dynamic_trampolines)
            .collect::<Vec<_>>();

        // TOOD: this shouldn't be necessary....
        let mut section_types = Vec::with_capacity(custom_sections.len());
        let mut executable_sections = Vec::new();
        let mut data_sections = Vec::new();
        for section in custom_sections {
            if let CustomSectionProtection::ReadExecute = section.protection {
                executable_sections.push(section);
            } else {
                data_sections.push(section);
            }
            section_types.push(section.protection);
        }

        // 1. Calculate the total size, that is:
        // - function body size, including all trampolines
        // -- windows unwind info
        // -- padding between functions
        // - executable section body
        // -- padding between executable sections
        // - padding until a new page to change page permissions
        // - data section body size
        // -- padding between data sections
        let page_size = rustix::param::page_size();
        let total_len = round_up(
            function_bodies.iter().fold(0, |acc, func| {
                round_up(
                    acc + function_allocation_size(*func),
                    ARCH_FUNCTION_ALIGNMENT.into(),
                )
            }) + executable_sections.iter().fold(0, |acc, exec| {
                round_up(acc + exec.bytes.len(), ARCH_FUNCTION_ALIGNMENT.into())
            }),
            page_size,
        ) + data_sections.iter().fold(0, |acc, data| {
            round_up(acc + data.bytes.len(), DATA_SECTION_ALIGNMENT.into())
        });

        let mut code_memory = code_memory_pool.get(total_len).map_err(|e| {
            CompileError::Resource(format!("could not allocate code memory: {}", e))
        })?;
        let mut code_writer = unsafe {
            // SAFETY: We just popped out a “free” code memory from an allocator pool.
            code_memory.writer()
        };

        let mut allocated_functions = vec![];
        let mut allocated_data_sections = vec![];
        let mut allocated_executable_sections = vec![];
        for func in function_bodies {
            let offset = code_writer
                .write_executable(ARCH_FUNCTION_ALIGNMENT, func.body)
                .expect("TODO");
            if let Some(CompiledFunctionUnwindInfoRef::WindowsX64(info)) = &func.unwind_info {
                // Windows unwind information is written following the function body
                // Keep unwind information 32-bit aligned (round up to the nearest 4 byte boundary)
                code_writer.write_executable(4, info).expect("TODO");
            }
            allocated_functions.push((offset, func.body.len()));
        }
        for section in executable_sections {
            let offset = code_writer
                .write_executable(ARCH_FUNCTION_ALIGNMENT, section.bytes)
                .expect("TODO");
            allocated_executable_sections.push(offset);
        }
        if !data_sections.is_empty() {
            // Data sections have different page permissions from the executable
            // code that came before it, so they need to be on different pages.
            let mut alignment = page_size as u16;
            for section in data_sections {
                let offset = code_writer
                    .write_aligned(alignment, section.bytes)
                    .expect("TODO");
                alignment = DATA_SECTION_ALIGNMENT;
                allocated_data_sections.push(offset);
            }
        }

        let mut allocated_function_call_trampolines: PrimaryMap<SignatureIndex, VMTrampoline> =
            PrimaryMap::new();
        for (offset, _) in allocated_functions.drain(0..call_trampoline_count) {
            // TODO: What in damnation have you done?! – Bannon
            let trampoline = unsafe {
                std::mem::transmute::<_, VMTrampoline>(code_memory.executable_address(offset))
            };
            allocated_function_call_trampolines.push(trampoline);
        }

        let allocated_functions_result = allocated_functions
            .drain(0..function_count)
            .enumerate()
            .map(|(index, (offset, length))| -> Result<_, CompileError> {
                let index = LocalFunctionIndex::new(index);
                let (sig_idx, sig) = function_signature(index);
                Ok(VMLocalFunction {
                    body: FunctionBodyPtr(code_memory.executable_address(offset).cast()),
                    length: u32::try_from(length).map_err(|_| {
                        CompileError::Codegen("function body length exceeds 4GiB".into())
                    })?,
                    signature: sig,
                    trampoline: allocated_function_call_trampolines[sig_idx],
                })
            })
            .collect::<Result<PrimaryMap<LocalFunctionIndex, _>, _>>()?;

        let allocated_dynamic_function_trampolines = allocated_functions
            .drain(..)
            .map(|(offset, _)| FunctionBodyPtr(code_memory.executable_address(offset).cast()))
            .collect::<PrimaryMap<FunctionIndex, _>>();

        let mut exec_iter = allocated_executable_sections.iter();
        let mut data_iter = allocated_data_sections.iter();
        let allocated_custom_sections = section_types
            .into_iter()
            .map(|protection| {
                SectionBodyPtr(if protection == CustomSectionProtection::ReadExecute {
                    code_memory
                        .executable_address(*exec_iter.next().unwrap())
                        .cast()
                } else {
                    code_memory
                        .writable_address(*data_iter.next().unwrap())
                        .cast()
                })
            })
            .collect::<PrimaryMap<SectionIndex, _>>();

        Ok((
            allocated_functions_result,
            allocated_function_call_trampolines,
            allocated_dynamic_function_trampolines,
            allocated_custom_sections,
            code_memory,
        ))
    }

    /// Shared func metadata registry.
    pub(crate) fn func_data(&self) -> &Arc<FuncDataRegistry> {
        &self.func_data
    }
}

fn round_up(size: usize, multiple: usize) -> usize {
    debug_assert!(multiple.is_power_of_two());
    (size + (multiple - 1)) & !(multiple - 1)
}

fn function_allocation_size(func: FunctionBodyRef<'_>) -> usize {
    match &func.unwind_info {
        Some(CompiledFunctionUnwindInfoRef::WindowsX64(info)) => {
            // Windows unwind information is required to be emitted into code memory
            // This is because it must be a positive relative offset from the start of the memory
            // Account for necessary unwind information alignment padding (32-bit alignment)
            ((func.body.len() + 3) & !3) + info.len()
        }
        _ => func.body.len(),
    }
}
