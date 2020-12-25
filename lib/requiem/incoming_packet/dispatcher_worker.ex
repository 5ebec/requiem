defmodule Requiem.IncomingPacket.DispatcherWorker do
  require Logger
  use GenServer

  @type t :: %__MODULE__{
          handler: module,
          transport: module,
          token_secret: binary,
          conn_id_secret: binary,
          trace: boolean
        }

  defstruct handler: nil,
            transport: nil,
            token_secret: "",
            conn_id_secret: "",
            trace: false

  @spec dispatch(pid, Requiem.Address.t(), iodata()) ::
          :ok | {:error, :timeout}
  def dispatch(pid, address, packet) do
    try do
      GenServer.call(pid, {:packet, address, packet}, 50)
      :ok
    catch
      :exit, _ ->
        Logger.error("<Requiem.IncomingPacket.DispatcherWorker> failed to dispatch packet")
        {:error, :timeout}
    end
  end

  @spec start_link(Keyword.t()) :: GenServer.on_start()
  def start_link(opts) do
    GenServer.start_link(__MODULE__, opts)
  end

  @impl GenServer
  def init(opts) do
    {:ok, new(opts)}
  end

  @impl GenServer
  def handle_call({:packet, address, packet}, _from, state) do
    case Requiem.QUIC.Packet.parse_header(packet) do
      {:ok, scid, dcid, _token, _version, false, _version_supported} ->
        debug("@regular", dcid, scid, "", state)
        handle_regular_packet(address, packet, scid, dcid, state)

      {:ok, scid, dcid, _token, _version, true, false} ->
        debug("@unsupported_version", dcid, scid, "", state)
        handle_version_unsupported_packet(address, scid, dcid, state)

      {:ok, scid, dcid, <<>>, version, true, true} ->
        debug("@token_missing", dcid, scid, "", state)
        handle_token_missing_packet(address, scid, dcid, version, state)

      {:ok, scid, dcid, token, _version, true, true} ->
        debug("@init_with_token", dcid, scid, "", state)
        handle_init_packet(address, packet, scid, dcid, token, state)

      {:error, reason} ->
        if state.trace do
          Logger.debug(
            "<Requiem.IncomingPacket.DispatcherWorker:#{inspect(self())}> bad formatted packet: #{
              inspect(reason)
            }"
          )
        end

        :ok
    end

    {:reply, :ok, state}
  end

  def handle_call(_ev, _from, state) do
    if state.trace do
      Logger.info(
        "<Requiem.IncomingPacket.DispatcherWorker:#{inspect(self())}> unknown handle_call pattern"
      )
    end

    {:reply, :ok, state}
  end

  defp new(opts) do
    %__MODULE__{
      handler: Keyword.fetch!(opts, :handler),
      transport: Keyword.fetch!(opts, :transport),
      token_secret: Keyword.fetch!(opts, :token_secret),
      conn_id_secret: Keyword.fetch!(opts, :conn_id_secret),
      trace: Keyword.get(opts, :trace, false)
    }
  end

  defp handle_regular_packet(address, packet, scid, dcid, state) do
    Requiem.ConnectionSupervisor.dispatch_packet(
      state.handler,
      address,
      packet,
      scid,
      dcid,
      state.trace
    )
  end

  defp handle_version_unsupported_packet(address, scid, dcid, state) do
    case Requiem.QUIC.Packet.build_negotiate_version(state.handler, scid, dcid) do
      {:ok, resp} ->
        state.transport.send(state.handler, address, resp)
        :ok

      error ->
        error
    end
  end

  defp handle_token_missing_packet(address, scid, dcid, version, state) do
    with {:ok, new_id} <-
           Requiem.QUIC.ConnectionID.generate_from_odcid(state.conn_id_secret, dcid),
         {:ok, token} <-
           Requiem.QUIC.RetryToken.create(address, dcid, new_id, state.token_secret),
         {:ok, resp} <-
           Requiem.QUIC.Packet.build_retry(state.handler, scid, dcid, new_id, token, version) do
      state.transport.send(state.handler, address, resp)
      :ok
    else
      {:error, _reason} -> :error
      :error -> :error
    end
  end

  defp handle_init_packet(address, packet, scid, dcid, token, state) when byte_size(dcid) == 20 do
    debug("@validate", dcid, scid, "", state)

    case Requiem.QUIC.RetryToken.validate(address, state.token_secret, token) do
      {:ok, odcid, _retry_scid} ->
        debug("@validate: success", dcid, scid, odcid, state)

        case Requiem.ConnectionSupervisor.create_connection(
               state.handler,
               state.transport,
               address,
               scid,
               dcid,
               odcid,
               state.trace
             ) do
          :ok ->
            handle_regular_packet(address, packet, scid, dcid, state)

          {:error, :system_error} ->
            :error
        end

      :error ->
        debug("@validate: error", dcid, scid, "", state)
        :error
    end
  end

  defp handle_init_packet(_address, _packet, scid, dcid, _token, state) do
    debug("@validate: bad dcid", dcid, scid, "", state)
    :error
  end

  defp debug(message, dcid, scid, odcid, %__MODULE__{trace: true}) do
    Logger.debug(
      "<Requiem.IncomingPacket.DispatcherWorker> #{message} <dcid:#{Base.encode16(dcid)}, scid: #{
        Base.encode16(scid)
      }, odcid: #{Base.encode16(odcid)}>"
    )
  end

  defp debug(_message, _dcid, _scid, _odcid, _state), do: :ok
end
