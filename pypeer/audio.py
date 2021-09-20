import logging

from PySide2.QtCore import QObject, Slot
from PySide2.QtMultimedia import QAudioDeviceInfo, QAudioFormat, QAudioOutput
from PySide2.QtNetwork import QUdpSocket


class PeerAudioPlayer(QObject):
    """Receives and plays audio from peers."""

    # TODO: Support for multiple peers (if necessary)

    def __init__(self, parent):
        super().__init__(parent)

        # Bind a UDP socket for listening on port 9898
        self._socket = QUdpSocket()
        self._socket.bind(9898)
        logging.info(
            f"Listening for peer audio on `{self._socket.localAddress().toIPv4Address()}:{self._socket.localPort()}`"
        )

        format = QAudioFormat()
        format.setSampleRate(44100)
        format.setChannelCount(2)
        format.setSampleSize(32)
        format.setCodec("audio/pcm")
        format.setByteOrder(QAudioFormat.LittleEndian)
        format.setSampleType(QAudioFormat.Float)

        info = QAudioDeviceInfo.defaultOutputDevice()
        if not info.isFormatSupported(format):
            format = info.nearestFormat(format)
        logging.info(
            f"Using audio output device `{info.deviceName()}` with format `{format}`"
        )

        output = QAudioOutput(format)
        self._device = output.start()

        # Log the output buffer size used, note that the actual buffer size is unknown until `start()` is called
        logging.debug(f"Using audio output buffer size of {output.bufferSize()} bytes")

        self._socket.readyRead.connect(self.play_data)

    @Slot()
    def play_data(self):
        # Read all pending datagrams from the socket and write their contents to the output device
        while self._socket.hasPendingDatagrams():
            datagram = self._socket.receiveDatagram()
            if datagram.isValid():
                self._device.write(datagram.data())
